#!/usr/bin/env python3
"""Inject real worker/main panics into the test-only ncm binary in owned PTYs. Use heavy."""
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import termios
import time

root = Path(__file__).resolve().parent.parent
out = root / 'target/ncm-panic-checks'
out.mkdir(parents=True, exist_ok=True)
candidates = [p for p in (root / 'target/debug/deps').glob('ncm-*')
              if p.is_file() and os.access(p, os.X_OK) and p.suffix == '']
env = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE', 'NCM_TMUX_SOCKET')}
env['TERM'] = 'xterm-256color'
# Cargo leaves both production and test executables in deps. Identify the test
# harness by its help rather than accidentally selecting a newer production build.
for binary in sorted(candidates, key=lambda p: p.stat().st_mtime, reverse=True):
    help_text = subprocess.check_output([str(binary), '--help'],
                                       env=dict(env, HOME=str(out / 'identify-home')))
    if b'--ignored' in help_text:
        break
else:
    raise RuntimeError('ncm test harness not found; run cargo test --workspace first')
socket = out / f'panic-{os.getpid()}.sock'
base = ['tmux', '-S', str(socket)]
results = {}
try:
    subprocess.run(base + ['-f', '/dev/null', 'new-session', '-d', '-s', 'panic', 'sleep 120'],
                   env=env, check=True)
    threads = ('main', 'ncm-collector', 'ncm-pictures', 'ncm-output', 'ncm-input')
    for thread in threads:
        (out / thread / '.local/state/ncm/panic.log').unlink(missing_ok=True)
    # Repeat the main case to verify append through the real hook, not just its writer.
    for thread in (*threads, 'main'):
        home = out / thread
        home.mkdir(exist_ok=True)
        log = home / '.local/state/ncm/panic.log'
        previous = log.read_bytes() if log.exists() else b''
        case_env = dict(env, HOME=str(home), NCM_TEST_PANIC=thread, RUST_BACKTRACE='0')
        if thread == 'ncm-collector':
            case_env.update(NCM_TEST_LIVE='1', NCM_TMUX_SOCKET=str(socket))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 42, 179, 1432, 672))
        before = termios.tcgetattr(slave)
        child = subprocess.Popen([str(binary), '--exact', 'panic_log::tests::terminal_child',
                                  '--ignored', '--nocapture'], stdin=slave, stdout=slave,
                                 stderr=slave, env=case_env, start_new_session=True)
        data = bytearray()
        try:
            deadline = time.monotonic() + 12
            while child.poll() is None:
                if time.monotonic() > deadline:
                    raise AssertionError(f'{thread}: hung after panic')
                if select.select([master], [], [], .025)[0]:
                    data.extend(os.read(master, 65536))
            child.wait()
            while select.select([master], [], [], .05)[0]:
                data.extend(os.read(master, 65536))
            after = termios.tcgetattr(slave)
            # macOS may set PENDIN when tcsetattr restores cooked input; it is
            # a kernel reprocessing marker rather than a raw-mode difference.
            before[3] &= ~getattr(termios, 'PENDIN', 0)
            after[3] &= ~getattr(termios, 'PENDIN', 0)
            (out / f'{thread}.pty').write_bytes(data)
            assert child.returncode == 1, (thread, child.returncode, data[-2000:])
            assert before == after, f'{thread}: terminal attributes not restored: {before!r} -> {after!r}'
            log_bytes = log.read_bytes()
            assert log_bytes.startswith(previous) and len(log_bytes) > len(previous), 'panic did not append'
            assert len(log_bytes) <= 256 * 1024
            evidence = log_bytes[len(previous):].decode()
            assert f'message=injected {thread} panic' in evidence
            assert 'timestamp_unix=' in evidence and 'location=' in evidence
            assert 'backtrace:' in evidence and 'panic_log::inject' in evidence, evidence
            expected_name = thread if thread != 'main' else 'panic_log::tests::terminal_child'
            assert f'thread={expected_name}' in evidence
            notice = f'panic in {expected_name}, see ~/.local/state/ncm/panic.log'.encode()
            restore = data.rfind(b'\x1b[?1049l')
            assert restore >= 0 and data.find(notice, restore) > restore
            if thread != 'main':
                dock = data.find(notice)
                assert 0 <= dock < restore, f'{thread}: missing dock notice before restore'
                assert b'\x1b[42;1H\x1b[2K' in data[:restore]
            assert b'\x1b[?1049h' not in data[restore:], 'redraw after restoration'
            results[thread] = {'code': child.returncode, 'raw_restored': True,
                               'evidence': str(log.relative_to(root)), 'dock': thread != 'main', 'append_verified': bool(previous)}
        finally:
            if child.poll() is None:
                child.kill()
            child.wait()
            os.close(master)
            os.close(slave)
finally:
    subprocess.run(base + ['kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    socket.unlink(missing_ok=True)
    (out / 'panic.json').write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(results, indent=2))
