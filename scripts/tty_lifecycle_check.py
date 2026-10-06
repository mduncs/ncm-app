#!/usr/bin/env python3
"""Own/reap every child; check lost tty and signals under input/output pressure. Use heavy."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import termios
import threading
import time

p=argparse.ArgumentParser();p.add_argument('--binary',default='target/debug/ncm');a=p.parse_args()
root=Path(__file__).resolve().parent.parent
out=root/'target'/'ncm-feel-checks';out.mkdir(parents=True,exist_ok=True)
result={}
for case in ('tty_eio','sighup_busy','sigterm_busy','sigterm_blocked_output'):
    master,slave=pty.openpty()
    fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',50,160,1280,800))
    env={k:v for k,v in os.environ.items() if k not in ('TMUX','TMUX_PANE','NCM_TMUX_SOCKET')};env['TERM']='xterm-256color'
    trace=out/f'{case}.log';trace.unlink(missing_ok=True)
    child=subprocess.Popen([str(root/a.binary),'--demo','--read-only','--log',str(trace)],stdin=slave,stdout=slave,stderr=slave,env=env,start_new_session=True)
    os.close(slave)
    done=threading.Event()
    def drain():
        while not done.is_set():
            try:
                if select.select([master],[],[],.02)[0]:os.read(master,65536)
            except (OSError,ValueError):return
    reader=threading.Thread(target=drain,daemon=True);reader.start()
    try:
        end=time.monotonic()+5
        while not trace.exists() or 'event=first_frame' not in trace.read_text():
            if child.poll() is not None:raise RuntimeError('early exit')
            if time.monotonic()>end:raise RuntimeError('startup timeout')
            time.sleep(.005)
        if case=='sigterm_blocked_output':done.set();reader.join(timeout=.1)
        if 'busy' in case:
            os.set_blocking(master,False)
            flood_stop=threading.Event()
            def flood():
                while not flood_stop.is_set():
                    try:os.write(master,b'\x1b[C\x1b[D'*512)
                    except OSError:time.sleep(.001)
            flooder=threading.Thread(target=flood,daemon=True);flooder.start();time.sleep(.04)
        elif case=='sigterm_blocked_output':
            os.write(master,b' '*50);time.sleep(.05)
        stamp=time.monotonic()
        if case=='tty_eio':
            done.set();reader.join(timeout=.1);os.close(master);master=None
        else:child.send_signal(signal.SIGHUP if case=='sighup_busy' else signal.SIGTERM)
        child.wait(timeout=1)
        result[case]={'exit_ms':(time.monotonic()-stamp)*1000,'code':child.returncode}
        assert child.returncode==0,result[case]
    finally:
        if 'busy' in case:flood_stop.set();flooder.join(timeout=.2)
        done.set();reader.join(timeout=.2)
        if master is not None:os.close(master)
        if child.poll() is None:child.kill()
        child.wait()
(out/'tty-lifecycle.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
