"""Reap via the pane-local run_owned parent, with bounded fallback cleanup."""
import os
from pathlib import Path
import signal
import subprocess
import time

def reap_from_trace(trace, root):
    if not Path(trace).exists():return
    pids=set()
    for row in Path(trace).read_text().splitlines():
        if 'event=start' in row:
            fields=dict(item.split('=',1) for item in row.split() if '=' in item)
            if 'pid' in fields:pids.add(int(fields['pid']))
    for pid in pids:
        def exists():
            try:os.kill(pid,0);return True
            except ProcessLookupError:return False
        if not exists():continue
        # Verify this harness's worktree executable before any fallback signal.
        command=subprocess.run(['ps','-p',str(pid),'-o','command='],capture_output=True,text=True).stdout
        if str(root / 'target') not in command or '/ncm' not in command:continue
        os.kill(pid,signal.SIGTERM)
        end=time.monotonic()+1
        while exists() and time.monotonic()<end:time.sleep(.01)
        if exists():
            os.kill(pid,signal.SIGKILL)
            end=time.monotonic()+1
            while exists() and time.monotonic()<end:time.sleep(.01)
        if exists():raise RuntimeError(f'harness child {pid} was not reaped by run_owned')
