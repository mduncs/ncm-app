#!/usr/bin/env python3
"""Two actual attached PTYs, kitty fake and xterm/nested-tmux fake. Use heavy."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import signal
import struct
import subprocess
import termios
import threading
import time

from process_cleanup import reap_from_trace

parser = argparse.ArgumentParser()
parser.add_argument('--binary', default='target/debug/ncm')
parser.add_argument('--cycles', type=int, default=12)
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
out = root / 'target/ncm-client-checks'
out.mkdir(parents=True, exist_ok=True)
socket = out / f'clients-{os.getpid()}.sock'
trace = out / 'clients.log'
trace.unlink(missing_ok=True)
home = out / 'home'
home.mkdir(exist_ok=True)
panic_log = home / '.local/state/ncm/panic.log'
panic_log.unlink(missing_ok=True)
env = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE', 'NCM_TMUX_SOCKET')}
env.update(TERM='xterm-ghostty', HOME=str(home))
base = ['tmux', '-S', str(socket)]

def tmux(*args):
    return subprocess.check_output(base + list(args), env=env, stderr=subprocess.PIPE).decode().strip()

def wait(predicate, label, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.025)
    raise AssertionError(f'{label}\n{trace.read_text()[-4000:] if trace.exists() else "no trace"}')

def log():
    return trace.read_text() if trace.exists() else ''

class Client:
    def __init__(self, term, kind, width, height):
        self.master, slave = pty.openpty()
        self.tty = os.ttyname(slave)
        self.kind = kind
        self.kitty = term == 'xterm-ghostty'
        self.done = threading.Event()
        self.wire = bytearray()
        self.lock = threading.Lock()
        self.images = set()
        self.queries = 0
        self.last_image = time.monotonic()
        self.child = None
        self.resize(width, height)
        self.child = subprocess.Popen(base + ['attach-session', '-t', 'clients'], stdin=slave,
                                      stdout=slave, stderr=slave, env=dict(env, TERM=term),
                                      start_new_session=True)
        os.close(slave)
        self.thread = threading.Thread(target=self.drain, daemon=True)
        self.thread.start()
        wait(lambda: self.tty in tmux('list-clients', '-F', '#{client_tty}'), 'client attach')
        wait(lambda: kind in tmux('list-clients', '-F', '#{client_termtype}'), 'fake XTVERSION identity')

    def resize(self, width, height):
        fcntl.ioctl(self.master, termios.TIOCSWINSZ,
                    struct.pack('HHHH', height, width, width * 8, height * 16))
        if self.child is not None and self.child.poll() is None:
            self.child.send_signal(signal.SIGWINCH)

    def drain(self):
        pending = b''
        image_id = None
        try:
            while not self.done.is_set():
                if not select.select([self.master], [], [], .025)[0]:
                    continue
                data = os.read(self.master, 65536)
                with self.lock:
                    self.wire.extend(data)
                pending += data
                # Answer tmux's terminal identification with the same identity as the laptop.
                replies = [(rb'\x1b\[>q', b'\x1bP>|' + self.kind.encode() + b'\x1b\\'),
                           (rb'\x1b\[c', b'\x1b[?1;2c'),
                           (rb'\x1b\[>c', b'\x1b[>0;0;0c')]
                consumed = 0
                for pattern, reply in replies:
                    for match in re.finditer(pattern, pending):
                        os.write(self.master, reply)
                        consumed = max(consumed, match.end())
                for match in re.finditer(rb'\x1b_G([^;\x1b]*);[^\x1b]*\x1b+\\', pending):
                    header = match.group(1)
                    consumed = max(consumed, match.end())
                    if b'a=q,' in header:
                        self.queries += 1
                        if self.kitty:
                            os.write(self.master, b'\x1b_Gi=255;OK\x1b\\\x1b[6;16;8t')
                    if b'a=T,' in header:
                        image_id = int(re.search(rb'(?:^|,)i=(\d+)', header).group(1))
                    if image_id is not None and re.search(rb'(?:^|,)m=[01](?:,|$)', header):
                        self.last_image = time.monotonic()
                        if re.search(rb'(?:^|,)m=0(?:,|$)', header):
                            self.images.add(image_id)
                            image_id = None
                if consumed:
                    pending = pending[consumed:]
                pending = pending[-16384:]
        except OSError:
            pass

    def snapshot(self):
        with self.lock:
            return bytes(self.wire)

    def latest(self):
        os.write(self.master, b'\x1b[D')

    def close(self):
        if self.child.poll() is None:
            self.child.terminate()
        try:
            self.child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.child.kill()
            self.child.wait()
        self.done.set()
        self.thread.join(timeout=1)
        os.close(self.master)

ghost = second = None
results = {'cycles': args.cycles, 'sizes': [], 'second_clients': [], 'zero_apc': True}
try:
    viewer = tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'clients', '-x', '255', '-y', '63',
                  '-P', '-F', '#{pane_id}', '/bin/sh')
    tmux('set-option', '-g', 'status', 'off')
    tmux('set-option', '-g', 'allow-passthrough', 'on')
    tmux('set-option', '-wg', 'window-size', 'latest')
    tmux('new-session', '-d', '-s', 'away', 'sleep 240')
    for index in range(1, 7):
        tmux('new-window', '-d', '-t', 'clients', '-n', f'source{index}', 'sleep 240')
    ghost = Client('xterm-ghostty', 'ghostty 1.2.3', 255, 63)
    command = shlex.join(['python3', str(root / 'scripts/run_owned.py'), str(root / args.binary),
                          '--read-only', '--log', str(trace)])
    tmux('send-keys', '-t', viewer, '-l', command)
    tmux('send-keys', '-t', viewer, 'Enter')
    wait(lambda: 'event=first_frame' in log(), 'startup')
    wait(lambda: ghost.images and 'graphics=true pending=false' in log(), 'kitty before second attach')
    wait(lambda: len(re.findall(r'event=picture_ready key=@', log())) >= 6
         and time.monotonic() - ghost.last_image > .6, 'initial complete upload')
    original_ids = set(ghost.images)
    assert len(original_ids) >= 6, original_ids
    results['initial_image_ids'] = sorted(original_ids)
    pid = int(re.search(r'event=start pid=(\d+)', log()).group(1))
    def alive():
        try:
            os.kill(pid, 0)
            return True
        except ProcessLookupError:
            return False
    def capture():
        return tmux('capture-pane', '-p', '-t', viewer)
    def tabs():
        for tab, needle in [('1', 'Screen'), ('2', 'Talk'), ('3', 'Reports')]:
            tmux('send-keys', '-t', viewer, 'Space', tab)
            wait(lambda: needle in capture(), f'{tab}: open {needle}')
            tmux('send-keys', '-t', viewer, 'Escape')
            time.sleep(.04)
            assert alive(), 'ncm died during tabs'
    for cycle in range(args.cycles):
        kind = 'tmux 3.5a' if cycle % 2 == 0 else 'xterm'
        second = Client('xterm-256color', kind, 179, 47)
        results['second_clients'].append(kind)
        wait(lambda: f'braille: {kind} client attached' in capture(), 'braille with incompatible client')
        # Include every byte since attach, before even the dock transition has settled.
        for active in (ghost, second, ghost, second):
            width, height = (255, 63) if active is ghost else (179, 42)
            active.resize(width, height)
            active.latest()
            wait(lambda: tmux('display-message', '-p', '-t', viewer,
                             '#{window_width}x#{window_height}') == f'{width}x{height}', 'latest client resize')
            results['sizes'].append([width, height])
            tabs()
            assert alive(), 'ncm died during resize'
        wire = second.snapshot()
        (out / f'second-{cycle}.pty').write_bytes(wire)
        assert b'\x1b_' not in wire and b'\x1bPtmux;' not in wire, f'APC/passthrough leaked on cycle {cycle}'
        before = ghost.queries
        ghost.images.clear()
        # Switching this one client to another session must restore graphics.
        tmux('switch-client', '-c', second.tty, '-t', 'away')
        ghost.latest()
        wait(lambda: ghost.queries > before and original_ids <= ghost.images,
             'full retransmit after client switches away')
        tmux('switch-client', '-c', second.tty, '-t', 'clients')
        wait(lambda: f'braille: {kind} client attached' in capture(), 'braille when client returns')
        time.sleep(.15)
        wire = second.snapshot()
        (out / f'second-{cycle}.pty').write_bytes(wire)
        assert b'\x1b_' not in wire and b'\x1bPtmux;' not in wire, 'APC leaked after return'
        before = ghost.queries
        ghost.images.clear()
        second.close()
        second = None
        ghost.latest()
        wait(lambda: ghost.queries > before and original_ids <= ghost.images,
             'full retransmit after incompatible client detaches')
        wait(lambda: tmux('display-message', '-p', '-t', viewer, '#{window_width}x#{window_height}') == '255x63', 'ghost latest after detach')
    assert not panic_log.exists() or not panic_log.read_text(), 'unexpected panic; see evidence'
    (out / 'final.txt').write_text(capture() + '\n')
    results.update(pid=pid, survived=True, unexpected_panics=0)
    tmux('send-keys', '-t', viewer, 'q')
    wait(lambda: not alive(), 'ncm reaped on quit', timeout=5)
    results['reaped'] = True
finally:
    reap_from_trace(trace, root)
    for client in (second, ghost):
        if client is not None:
            client.close()
    subprocess.run(base + ['kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    socket.unlink(missing_ok=True)
    (out / 'clients.json').write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(results, indent=2))
