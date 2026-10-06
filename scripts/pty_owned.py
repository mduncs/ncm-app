#!/usr/bin/env python3
"""Keep the controlling PTY alive long enough to verify modes, then reap the owned command."""
import fcntl
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import termios

report = Path(sys.argv[1])
child = None
stopping = False


def modes():
    attrs = termios.tcgetattr(0)
    attrs[3] &= ~getattr(termios, 'PENDIN', 0)
    return attrs


def stop(signum, _):
    global stopping
    stopping = True
    if child is not None and child.poll() is None:
        child.send_signal(signum)


for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
    signal.signal(sig, stop)
before = modes()
flags = fcntl.fcntl(1, fcntl.F_GETFL)
try:
    child = subprocess.Popen(sys.argv[2:])
    if stopping:
        child.terminate()
    while True:
        try:
            code = child.wait(timeout=.1)
            break
        except subprocess.TimeoutExpired:
            if stopping:
                try:
                    code = child.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    child.kill()
                    code = child.wait()
                break
    report.write_text(json.dumps({'pid': child.pid, 'code': code, 'restored': modes() == before,
                                  'flags_restored': (fcntl.fcntl(1, fcntl.F_GETFL) & os.O_NONBLOCK) == (flags & os.O_NONBLOCK)}) + '\n')
    sys.exit(code if code >= 0 else 128 - code)
finally:
    if child is not None and child.poll() is None:
        child.kill()
        child.wait()
