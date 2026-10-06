#!/usr/bin/env python3
"""Direct terminal graphics, grouped attach/return, and legacy fallback in owned PTYs. Use heavy."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import pwd
import re
import select
import shlex
import signal
import struct
import subprocess
import sys
import termios
import threading
import time

root = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser()
parser.add_argument('--binary', default='target/debug/ncm')
args = parser.parse_args()
out = root / 'target/ncm-outside-checks'
out.mkdir(parents=True, exist_ok=True)
binary = root / args.binary
homes = (os.path.expanduser('~'), pwd.getpwuid(os.getuid()).pw_dir)
tmux_bin = next((p for p in (*(os.path.join(h, '.local/bin/tmux') for h in homes), '/opt/homebrew/bin/tmux')
                 if os.access(p, os.X_OK)), 'tmux')
env = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE', 'NCM_TMUX_SOCKET')}
# Exercise an SSH-like environment with no user/Homebrew tools on PATH.
env.update(TERM='xterm-256color', PATH='/usr/bin:/bin', HOME=str(out / 'home'))
(out / 'home').mkdir(exist_ok=True)
socket = root / f'.ncm-outside-{os.getpid()}.sock'
base = [tmux_bin, '-S', str(socket)]
results = {}
clients = []


def tmux(*words):
    result = subprocess.run(base + list(words), env=env, capture_output=True)
    assert result.returncode == 0, (words, result.stderr.decode())
    return result.stdout.decode().strip()


def wait(predicate, label, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.02)
    raise AssertionError(label)


def modes(fd):
    attrs = termios.tcgetattr(fd)
    attrs[3] &= ~getattr(termios, 'PENDIN', 0)
    return attrs


class Terminal:
    def __init__(self, name, command, reply=True, delay=0, extra_env=None):
        self.name = name
        self.trace = out / f'{name}.log'
        self.trace.unlink(missing_ok=True)
        self.master, self.slave = pty.openpty()
        self.resize(120, 40)
        self.before = modes(self.slave)
        self.wire = bytearray()
        self.pending = b''
        self.images = set()
        self.queries = 0
        self.reply = reply
        self.delay = delay
        self.reply_at = None
        self.image_id = None
        self.done = threading.Event()
        self.error = None

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        self.report = out / f'{name}-owned.json'
        self.report.unlink(missing_ok=True)
        owned = [sys.executable, str(root / 'scripts/pty_owned.py'), str(self.report), *command]
        self.child = subprocess.Popen(owned, stdin=self.slave, stdout=self.slave, stderr=self.slave,
                                      env=dict(env, **(extra_env or {})), preexec_fn=controlling_terminal)
        clients.append(self)
        self.thread = threading.Thread(target=self.drain, daemon=True)
        self.thread.start()

    def resize(self, width, height):
        fcntl.ioctl(self.master, termios.TIOCSWINSZ,
                    struct.pack('HHHH', height, width, width * 8, height * 16))
        if hasattr(self, 'child') and self.child.poll() is None:
            self.child.send_signal(signal.SIGWINCH)

    def drain(self):
        try:
            while not self.done.is_set():
                if self.reply_at is not None and time.monotonic() >= self.reply_at:
                    os.write(self.master, b'\x1b_Gi=255;OK\x1b\\\x1b[6;16;8t')
                    self.reply_at = None
                if not select.select([self.master], [], [], .02)[0]:
                    continue
                data = os.read(self.master, 65536)
                self.wire.extend(data)
                self.pending += data
                consumed = 0
                # The real tmux client may ask DA/XTVERSION while it owns the terminal.
                for pattern, response in ((rb'\x1b\[>q', b'\x1bP>|XTerm(400)\x1b\\'),
                                          (rb'\x1b\[c', b'\x1b[?1;2c'),
                                          (rb'\x1b\[>c', b'\x1b[>0;0;0c')):
                    for match in re.finditer(pattern, self.pending):
                        os.write(self.master, response)
                        consumed = max(consumed, match.end())
                for match in re.finditer(rb'\x1b_G([^;\x1b]*);[^\x1b]*\x1b\\', self.pending):
                    header = match.group(1)
                    consumed = max(consumed, match.end())
                    if b'a=q,' in header:
                        self.queries += 1
                        if self.reply:
                            self.reply_at = time.monotonic() + self.delay
                    if b'a=T,' in header:
                        self.image_id = int(re.search(rb'(?:^|,)i=(\d+)', header).group(1))
                    if self.image_id is not None and re.search(rb'(?:^|,)m=0(?:,|$)', header):
                        self.images.add(self.image_id)
                        self.image_id = None
                if consumed:
                    self.pending = self.pending[consumed:]
                self.pending = self.pending[-16384:]
        except (OSError, ValueError) as error:
            if not self.done.is_set():
                self.error = str(error)

    def log(self):
        return self.trace.read_text() if self.trace.exists() else ''

    def send(self, data):
        os.write(self.master, data)

    def stop(self):
        if self.child.poll() is None:
            self.child.send_signal(signal.SIGTERM)
        try:
            self.child.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.child.kill()
            self.child.wait()
        time.sleep(.05)
        self.done.set()
        self.thread.join(timeout=1)
        (out / f'{self.name}.pty').write_bytes(self.wire)
        os.close(self.master)
        os.close(self.slave)


def ncm(name, extra=(), **kwargs):
    trace = out / f'{name}.log'
    return Terminal(name, [str(binary), '--log', str(trace), *extra], **kwargs)


def no_wrappers(client):
    assert b'\x1bPtmux;' not in client.wire, 'tmux passthrough appeared on the wire'


try:
    delayed = ncm('delayed', ('--demo', '--read-only'), delay=1.4)
    wait(lambda: delayed.queries == 1, 'startup kitty query')
    delayed.send(b'\x1b[C')
    wait(lambda: 'event=input_painted selected=1' in delayed.log(), 'input while probing')
    assert 'graphics=true pending=false' not in delayed.log()
    wait(lambda: len(delayed.images) >= 20, 'delayed SSH-like reply enables all card images')
    assert delayed.queries == 1
    initial = set(delayed.images)
    delayed.images.clear()
    delayed.resize(100, 32)
    wait(lambda: len(delayed.images) > 0, 'resize retransmits images')
    delayed.images.clear()
    delayed.resize(120, 40)
    wait(lambda: initial <= delayed.images, 'resize back replaces every card image')
    assert delayed.queries == 1
    delayed.send(b'q')
    delayed.child.wait(timeout=3)
    assert json.loads(delayed.report.read_text())["restored"]
    no_wrappers(delayed)
    results['direct_delayed_graphics_and_resize'] = True
    delayed.stop()
    clients.remove(delayed)

    fallback = ncm('fallback', ('--demo', '--read-only'), reply=False)
    wait(lambda: 'event=probe_timeout' in fallback.log(), 'single generous timeout', timeout=8)
    wait(lambda: b'\xe2\xa0' in fallback.wire or b'\xe2\xa1' in fallback.wire,
         'braille fallback')
    time.sleep(.2)
    assert fallback.queries == 1 and not fallback.images
    fallback.send(b'q')
    fallback.child.wait(timeout=3)
    no_wrappers(fallback)
    results['single_probe_braille_fallback'] = True
    fallback.stop()
    clients.remove(fallback)

    # Every action below is confined to this private server.
    tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'main', 'sleep 120')
    source = tmux('display-message', '-p', '-t', '=main:', '#{window_id}')
    input_file = out / 'pane-input.bin'
    input_file.write_bytes(b'')
    source_input = out / 'source-input.bin'
    source_input.write_bytes(b'')

    def pane_command(label, path):
        code = (f"import os; print('{label}', flush=True); "
                f"f=open({str(path)!r},'ab',buffering=0); "
                "\nwhile True:\n b=os.read(0,4096)\n if not b: break\n f.write(b)\n os.write(1,b)")
        return 'stty raw -echo; exec /usr/bin/python3 -u -c ' + shlex.quote(code)

    tmux('respawn-pane', '-k', '-t', '=main:', pane_command('SOURCE', source_input))
    target = tmux('new-window', '-d', '-P', '-F', '#{window_id}', '-t', 'main', '-n', 'target',
                  pane_command('TARGET', input_file))
    tmux('set-option', '-t', 'main', 'mouse', 'on')
    monitor = Terminal('other-client', base + ['attach-session', '-t', '=main'], reply=False)
    wait(lambda: tmux('list-clients', '-F', '#{?client_control_mode,,#{session_name}}') == 'main', 'other client attached')
    query_log = out / 'live-query-spawns.log'
    query_log.unlink(missing_ok=True)
    live = ncm('live', ('--socket', str(socket), '--session', 'main'),
               extra_env={'NCM_TMUX_QUERY_LOG': str(query_log)})
    wait(lambda: len(live.images) > 0 and 'TARGET' not in live.log(), 'live cards uploaded')
    assert len(query_log.read_text().splitlines()) == 1, 'source queries spawned extra clients'
    click_card = b'\x1b[<0;91;11M\x1b[<0;91;11m'
    live.send(click_card)
    wait(lambda: 'event=input_painted selected=1 quick=true' in live.log(),
         'card click selects and previews')
    wait(lambda: any(i >= 200 for i in live.images), 'preview images uploaded')
    expected_images = {int(i) for i in re.findall(
        rb'\x1b\[0;38;5;(\d+)m\xf4\x8e\xbb\xae', bytes(live.wire))}
    assert any(i >= 200 for i in expected_images)
    wait(lambda: expected_images <= live.images, 'all initial placements uploaded')
    helper = '_ncm_view_' + re.search(r'event=start pid=(\d+)', live.log()).group(1)

    def sessions():
        return tmux('list-sessions', '-F', '#{session_name}').splitlines()

    def opened(key):
        live.send(key)
        wait(lambda: helper in sessions() and helper in tmux('list-clients', '-F', '#{session_name}'),
             'grouped full-screen client attached')
        assert tmux('display-message', '-p', '-t', '=main:', '#{window_id}') == source
        assert tmux('display-message', '-p', '-t', f'={helper}:', '#{window_id}') == target
        assert tmux('show-options', '-v', '-t', helper, 'destroy-unattached') == 'on'
        assert 'ncm · C-] back' in tmux('show-options', '-v', '-t', helper, 'status-right')
        assert b'\x1b[?1049l' in live.wire, 'ncm did not leave alternate screen before attach'
        before = live.log().count('event=paint ')
        time.sleep(.15)
        assert live.log().count('event=paint ') == before, 'ncm painted over tmux'
        assert monitor.child.poll() is None

    def returned(key):
        live.images.clear()
        wire_start = len(live.wire)
        closed = live.log().count('event=use_closed')
        live.send(key)
        wait(lambda: live.log().count('event=use_closed') > closed, 'detach returns to ncm')
        wait(lambda: helper not in sessions(), 'grouped session cleaned on detach')
        wait(lambda: expected_images <= live.images, 'all grid and preview images retransmitted')
        assert b'\x1b[?1049h\x1b[2J' in live.wire[wire_start:], 'alternate screen not reentered'
        assert live.queries == 1, 'return unnecessarily reprobed kitty'
        assert tmux('display-message', '-p', '-t', '=main:', '#{window_id}') == source

    opened(b'\r')
    live.send(b'REAL-INPUT')
    wait(lambda: b'REAL-INPUT' in input_file.read_bytes(), 'real client forwards native typing')
    returned(b'\x1d')
    opened(b'i')
    returned(b'\x02d')
    # Installed root binding forwards C-] to ordinary sessions.
    monitor.send(b'\x1d')
    wait(lambda: b'\x1d' in source_input.read_bytes(), 'C-] passes through outside helper')
    assert monitor.child.poll() is None
    opened(b'\x1b[<0;61;21M\x1b[<0;61;21m')
    live.child.send_signal(signal.SIGTERM)
    live.child.wait(timeout=3)
    wait(lambda: helper not in sessions(), 'SIGTERM cleans helper and client')
    assert tmux('list-clients', '-F', '#{?client_control_mode,,#{session_name}}') == 'main'
    assert json.loads(live.report.read_text())['restored'], 'raw modes not restored on exit from attach'
    assert json.loads(live.report.read_text())['flags_restored']
    no_wrappers(live)
    results['grouped_attach_return_images_native_input_and_cleanup'] = True
    live.stop()
    clients.remove(live)

    # Legacy ncm is directly exercised with TMUX set, even if a hostile kitty reply arrives.
    legacy = ncm('legacy', ('--socket', str(socket), '--session', 'main'),
                 extra_env={'TMUX': f'{socket},0,0'})
    wait(lambda: 'event=first_frame' in legacy.log(), 'legacy first frame')
    legacy.send(b'\x1b_Gi=255;OK\x1b\\')
    legacy.send(b'\x1b[C\r')
    wait(lambda: tmux('display-message', '-p', '-t', '=main:', '#{window_id}') == target,
         'legacy Enter selects in source session')
    legacy.send(b'i')
    time.sleep(.2)
    assert not any(s.startswith('_ncm_view_') for s in sessions())
    assert not legacy.queries and not legacy.images and b'\x1b_G' not in legacy.wire
    assert b'\x1bPtmux;' not in legacy.wire
    legacy.send(b'q')
    legacy.child.wait(timeout=3)
    assert json.loads(legacy.report.read_text())['restored']
    results['legacy_zero_graphics_and_select'] = True
    legacy.stop()
    clients.remove(legacy)
finally:
    for client in list(clients):
        client.stop()
    subprocess.run(base + ['kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    socket.unlink(missing_ok=True)
    (out / 'outside.json').write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(results, indent=2))
