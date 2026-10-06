#!/usr/bin/env python3
"""A pane-local parent that always terminates and reaps the ncm it starts."""
import os
import signal
import subprocess
import sys

child = None
stopping = False
def stop(signum, _):
    global stopping
    stopping = True
    if child is not None:
        try: child.send_signal(signum)
        except ProcessLookupError: pass
for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
    signal.signal(sig, stop)
try:
    child = subprocess.Popen(sys.argv[1:])
    if stopping: child.terminate()
    while True:
        try: code = child.wait(timeout=.1); break
        except subprocess.TimeoutExpired:
            if stopping:
                try: code = child.wait(timeout=.7)
                except subprocess.TimeoutExpired: child.kill(); code = child.wait()
                break
    sys.exit(code if code >= 0 else 128-code)
finally:
    if child is not None and child.poll() is None:
        child.kill(); child.wait()
