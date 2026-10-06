#!/usr/bin/env python3
"""Injected-key paint timings; source socket is read-only, viewer is a private server.

Use heavy. Kitty replies are emulated: stdout flush is measured, not a GUI display.
No live captures/transcripts are written to disk.
"""
import argparse
from process_cleanup import reap_from_trace
import json
import fcntl
import pty
import select
import struct
import termios
import threading
import os
from pathlib import Path
import shlex
import subprocess
import time

p=argparse.ArgumentParser()
p.add_argument('--socket',default='/private/tmp/tmux-501/default')
p.add_argument('--session',default='main')
p.add_argument('--binary',default='target/release/ncm')
p.add_argument('--samples',type=int,default=40)
a=p.parse_args()
root=Path(__file__).resolve().parent.parent
out=root/'target'/'ncm-feel-checks';out.mkdir(parents=True,exist_ok=True)
socket=out/f'latency-{os.getpid()}.sock'
trace=out/'latency.log';trace.unlink(missing_ok=True)
env={k:v for k,v in os.environ.items() if k not in ('TMUX','TMUX_PANE','NCM_TMUX_SOCKET')}
base=['tmux','-S',str(socket)]
def tmux(*args):return subprocess.check_output(base+list(args),env=env,stderr=subprocess.PIPE).decode().strip()
def rows():
    result=[]
    if trace.exists():
        for row in trace.read_text().splitlines():
            fields=dict(token.split('=',1) for token in row.split() if '=' in token)
            if fields.get('mono_ns','').isdigit() and 'event' in fields:result.append(fields)
    return result
def await_event(event,after,timeout=5,condition=lambda r:True):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        for row in rows():
            if row.get('event')==event and int(row['mono_ns'])>=after and condition(row):return row
        time.sleep(.002)
    raise RuntimeError(f'timed out: {event}')
def inject(*keys):
    encoded={'Right':b'\x1b[C','Left':b'\x1b[D','Space':b' ','q':b'q'}
    stamp=time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    os.write(master,b''.join(encoded[key] for key in keys))
    return stamp
def summary(values):
    values=sorted(values)
    return {'samples':len(values),'p50_ms':values[len(values)//2],'p95_ms':values[min(len(values)-1,int(len(values)*.95))],'max_ms':max(values)}
result={}
master=client=None
finished=threading.Event()
def drain():
    probe_answered=False
    carry=b''
    while not finished.is_set():
        try:
            if select.select([master],[],[],.02)[0]:
                data=os.read(master,65536)
                if not data:return
                carry=(carry+data)[-4096:]
                if not probe_answered and b'_Ga=q' in carry and b'i=255' in carry:
                    os.write(master,b'\x1b_Gi=255;OK\x1b\\\x1b[6;16;8t')
                    probe_answered=True
                if b'\x1b[c' in data or b'\x1b[0c' in data:
                    os.write(master,b'\x1b[?62;4;22c')
                if b'\x1b[>c' in data or b'\x1b[>0c' in data:
                    os.write(master,b'\x1b[>1;4000;0c')
                if b'\x1b[16t' in data:os.write(master,b'\x1b[6;16;8t')
        except (OSError,ValueError):return

try:
    tmux('-f','/dev/null','new-session','-d','-s','viewer','-x','255','-y','60','sleep 120')
    tmux('set-option','-g','allow-passthrough','on')
    command=shlex.join(['python3',str(root/'scripts'/'run_owned.py'),str(root/a.binary),'--read-only','--socket',a.socket,'--session',a.session,'--log',str(trace)])
    viewer=tmux('new-window','-P','-F','#{pane_id}','-n','ncm','sleep 120')
    master,slave=pty.openpty()
    fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',60,255,2040,960))
    client=subprocess.Popen(base+['attach-session','-t','viewer'],stdin=slave,stdout=slave,stderr=slave,env={**env,'TERM':'xterm-256color'},start_new_session=True)
    os.close(slave)
    threading.Thread(target=drain,daemon=True).start()
    tmux('respawn-pane','-k','-t',viewer,command)
    await_event('probe_start',0)
    await_event('capabilities',0,condition=lambda r:r.get('graphics')=='true')
    await_event('poll',0);time.sleep(2)
    arrows=[];space=[];picture=[]
    for i in range(a.samples):
        stamp=inject('Right' if i%2==0 else 'Left')
        row=await_event('input_painted',stamp)
        arrows.append((int(row['mono_ns'])-stamp)/1e6)
        time.sleep(.025)
    # Held arrows must produce one paint after a burst instead of a backlog of frames.
    stamp=inject(*(['Right']*20+['Left']*20))
    row=await_event('input_painted',stamp);result['held_burst_ms']=(int(row['mono_ns'])-stamp)/1e6
    time.sleep(.2)
    for i in range(a.samples):
        stamp=inject('Space')
        row=await_event('input_painted',stamp,condition=lambda r:r.get('quick')=='true')
        space.append((int(row['mono_ns'])-stamp)/1e6)
        row=await_event('quick_picture',stamp)
        picture.append((int(row['mono_ns'])-stamp)/1e6)
        inject('Space');time.sleep(.06)
    stamp=inject('Space');await_event('quick_picture',stamp);time.sleep(8)
    inject('Space');time.sleep(.15)
    tmux('select-window','-t','viewer:0')
    hidden=await_event('collector_hidden',time.clock_gettime_ns(time.CLOCK_MONOTONIC)-500_000_000)
    time.sleep(1.1)
    hidden_start=time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    time.sleep(5)
    hidden_end=time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    hidden_rows=[r for r in rows() if hidden_start<=int(r['mono_ns'])<=hidden_end]
    hidden_samples=[r for r in hidden_rows if r['event']=='sample']
    result['hidden']={'polls':sum(r['event']=='poll' for r in hidden_rows),'facts':sum(r['event']=='facts' for r in hidden_rows),'image_flushes':sum(r['event']=='image_flush' for r in hidden_rows),'cpu_mean_pct':sum(float(r['cpu_pct']) for r in hidden_samples)/len(hidden_samples),'child_cpu_mean_pct':sum(float(r['child_cpu_pct']) for r in hidden_samples)/len(hidden_samples)}
    assert result['hidden']['polls']==result['hidden']['facts']==result['hidden']['image_flushes']==0,result['hidden']
    resume=time.clock_gettime_ns(time.CLOCK_MONOTONIC);tmux('select-window','-t',viewer)
    refreshed=await_event('poll',resume)
    result['resume_capture_ms']=(int(refreshed['mono_ns'])-resume)/1e6
    inject('q');time.sleep(.2)
    samples=[float(r['image_kbs']) for r in rows() if r['event']=='sample' and stamp+1_000_000_000<int(r['mono_ns'])<hidden_start-1_100_000_000]
    result.update({'arrow_to_painted':summary(arrows),'space_to_panel_painted':summary(space),'space_to_picture_flush':summary(picture),'quick_open_image_kbs_max':max(samples,default=0),'emulated_graphics':True,'injection':'outer client pty; includes tmux delivery'})
    (out/'latency.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
finally:
    reap_from_trace(trace,root)
    finished.set()
    if client:client.terminate();client.wait(timeout=1)
    if master is not None:os.close(master)
    subprocess.run(base+['kill-server'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    socket.unlink(missing_ok=True)
