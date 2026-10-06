#!/usr/bin/env python3
"""Real tmux client/popup lifecycle checks on a private server. Run through heavy."""
import argparse
from process_cleanup import reap_from_trace
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import shlex
import signal
import struct
import subprocess
import termios
import threading
import time

parser = argparse.ArgumentParser()
parser.add_argument('--binary', default='target/debug/ncm')
parser.add_argument('--occupied-back-key',action='store_true')
parser.add_argument('--clipboard-fix-probe',action='store_true')
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
out = root / 'target' / 'ncm-feel-checks'
out.mkdir(parents=True, exist_ok=True)
socket = out / f'use-{os.getpid()}.sock'
name='use-mode-clipboard' if args.clipboard_fix_probe else ('use-mode-occupied' if args.occupied_back_key else 'use-mode')
trace = out / f'{name}.log'
trace.unlink(missing_ok=True)
save_log=out/'detach-sessions.txt';save_log.unlink(missing_ok=True)
save_script=out/'private-save.sh'
save_script.write_text(shlex.join(['tmux','-S',str(socket),'list-sessions','-F','#{session_name}'])+' >> '+shlex.quote(str(save_log))+'\n')
env = {k:v for k,v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE', 'NCM_TMUX_SOCKET')}
env['TERM'] = 'xterm-256color'
base = ['tmux', '-S', str(socket)]
def tmux(*args):
    return subprocess.check_output(base + list(args), env=env, stderr=subprocess.PIPE).decode().strip()
def wait(test, label, timeout=5):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        if test(): return
        time.sleep(.02)
    raise AssertionError(label)
def sessions(): return tmux('list-sessions','-F','#{session_name}').splitlines()
def size(): return tmux('display-message','-p','-t',target,'#{window_width}x#{window_height}')
master=client=None
received=bytearray()
running=True
def drain():
    while running:
        if select.select([master],[],[],.05)[0]:
            try: data=os.read(master,65536)
            except OSError: return
            # Retain only protocol observations, never captured live pane contents.
            if b'\x1b]52;' in data: received.extend(data)
            if not data:return
results={}
before_clip=None
try:
    target=tmux('-f','/dev/null','new-session','-d','-s','source','-x','120','-y','40','-P','-F','#{pane_id}', "printf 'CLIPBOARD_PROBE selection text\\n'; stty -echo; cat")
    tmux('set-hook','-g','client-detached[80]','run-shell -b '+shlex.quote('sh '+shlex.quote(str(save_script))))
    if args.occupied_back_key:tmux('bind-key','-n','C-]','send-keys','C-]')
    original_binding=tmux('list-keys','-T','root','C-]') if args.occupied_back_key else None
    tmux('set-option','-g','mouse','on')
    tmux('set-option','-g','prefix','C-Space')
    tmux('set-option','-t','source','status','5')
    tmux('set-option','-g','window-size','latest')
    tmux('set-option','-s','set-clipboard','external')
    if args.clipboard_fix_probe:tmux('set-option','-s','copy-command','pbcopy')
    tmux('set-option','-g','allow-passthrough','on')
    # Advertise OSC52 support, just as a real terminal does.
    tmux('set-option','-as','terminal-overrides',',xterm*:Ms=\\E]52;%p1%s;%p2%s\\007')
    command=shlex.join(['python3',str(root/'scripts'/'run_owned.py'),str(root / args.binary),'--session','source','--log',str(trace)])
    viewer=tmux('new-window','-d','-n','viewer','-P','-F','#{pane_id}','sleep 120')
    master,slave=pty.openpty()
    fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,120,960,640))
    client=subprocess.Popen(base+['attach-session','-t','source'],stdin=slave,stdout=slave,stderr=slave,env=env,start_new_session=True)
    os.close(slave)
    thread=threading.Thread(target=drain,daemon=True);thread.start()
    time.sleep(.2)
    tmux('select-window','-t',target)
    time.sleep(.2)
    tmux('select-window','-t',viewer)
    # Start ncm only once the viewer has an attached client for identity resolution.
    trace.unlink(missing_ok=True)
    tmux('respawn-pane','-k','-t',viewer,command)
    wait(lambda: trace.exists() and 'event=first_frame' in trace.read_text() and 'event=poll' in trace.read_text(),'ncm startup',timeout=10)
    time.sleep(.35)
    initial=size()
    if args.occupied_back_key:
        results['existing_binding_preserved']=tmux('list-keys','-T','root','C-]')==original_binding
        assert results['existing_binding_preserved']
    back=b'\x1b]' if args.occupied_back_key else b'\x1d'
    def open_use():
        os.write(master,b'i')
        wait(lambda:any(s.startswith('_ncm_peek_') for s in sessions()),'helper not created')
        wait(lambda:len(tmux('list-clients','-F','#{client_name}').splitlines())==2,'popup nested client did not attach')
        time.sleep(.15)
        helper=next(s for s in sessions() if s.startswith('_ncm_peek_'))
        return helper
    helper=open_use()
    results['dimensions']={'before':initial,'during':size()}
    results['helper_status']=tmux('show-options','-v','-t',helper,'status')
    # Test actual mouse drag and inspect the tmux buffer plus OSC52 delivery.
    before_clip=subprocess.check_output(['pbpaste'])
    os.write(master,b'\x1b[<0;1;1M');time.sleep(.1)
    os.write(master,b'\x1b[<32;22;1M');time.sleep(.1)
    os.write(master,b'\x1b[<0;22;1m');time.sleep(.2)
    try: results['drag_buffer']=tmux('show-buffer')
    except subprocess.CalledProcessError: results['drag_buffer']=None
    results['osc52_reached_outer_pty']=bool(received)
    copied=subprocess.check_output(['pbpaste'])
    results['pbpaste_changed']=copied!=before_clip
    results['private_copy_command_pbcopy']=args.clipboard_fix_probe
    if args.clipboard_fix_probe:
        results['clipboard_fix_verified']=b'CLIPBOARD_PROBE' in copied
        assert results['clipboard_fix_verified']
    os.write(master,back)
    wait(lambda:helper not in sessions(),'C-] helper cleanup')
    wait(lambda:len(tmux('list-clients','-F','#{client_name}').splitlines())==1,'popup did not close')
    results['dimensions']['after']=size()
    results['ctrl_bracket_close']=True
    time.sleep(.35)
    helper=open_use();os.write(master,b'\x00d')
    wait(lambda:helper not in sessions(),'prefix-d helper cleanup')
    results['prefix_d_close']=True
    time.sleep(.35)
    helper=open_use()
    pid=int(helper.rsplit('_',1)[1]);stopped=time.monotonic();os.kill(pid,signal.SIGTERM)
    wait(lambda:helper not in sessions(),'SIGTERM helper cleanup',timeout=1)
    results['sigterm_helper_cleanup_ms']=(time.monotonic()-stopped)*1000
    results['sigterm_cleanup']=True
    # Outside helpers C-] must become input, not close source. Capture stty raw to see it.
    tmux('select-window','-t',target)
    tmux('respawn-pane','-k','-t',target,"stty raw -echo; od -An -t x1 -N 1; sleep 10")
    time.sleep(.15);os.write(master,b'\x1d')
    wait(lambda:'1d' in tmux('capture-pane','-p','-t',target),'root C-] did not pass through')
    results['root_pass_through']=True
    time.sleep(.2)
    results['detach_save_captured_helper']=save_log.exists() and '_ncm_peek_' in save_log.read_text()
    results['size_invariance']=len(set(results['dimensions'].values()))==1
    (out/f'{name}.json').write_text(json.dumps(results,indent=2)+'\n')
    print(json.dumps(results,indent=2))
    assert results['size_invariance'], 'target resized in popup'
finally:
    if args.clipboard_fix_probe and before_clip is not None:subprocess.run(['pbcopy'],input=before_clip,check=True)
    reap_from_trace(trace,root)
    running=False
    if client:client.terminate();client.wait()
    if master is not None:os.close(master)
    subprocess.run(base+['kill-server'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    socket.unlink(missing_ok=True)
    (out/f'{name}.json').write_text(json.dumps(results,indent=2)+'\n')
