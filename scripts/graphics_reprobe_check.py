#!/usr/bin/env python3
"""Headless graphics negotiation and reattach checks in an owned private tmux. Use heavy."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import struct
import subprocess
import termios
import threading
import time

from process_cleanup import reap_from_trace

parser = argparse.ArgumentParser()
parser.add_argument('--binary', default='target/debug/ncm')
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
out = root / 'target' / 'ncm-reprobe-checks'
out.mkdir(parents=True, exist_ok=True)
socket = out / f'graphics-{os.getpid()}.sock'
trace = out / 'graphics.log'
trace.unlink(missing_ok=True)
traces = [trace]
env = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE', 'NCM_TMUX_SOCKET')}
env['TERM'] = 'xterm-ghostty'
base = ['tmux', '-S', str(socket)]


def tmux(*arguments):
    return subprocess.check_output(base + list(arguments), env=env, stderr=subprocess.PIPE).decode().strip()


def wait(predicate, label, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.025)
    raise AssertionError(f'{label}\n{trace.read_text() if trace.exists() else "no trace"}')


def log():
    return trace.read_text() if trace.exists() else ''


class Client:
    """A PTY terminal that optionally answers actual tmux passthrough queries."""
    def __init__(self, delay=0, answer_on=1):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 960, 640))
        self.child = subprocess.Popen(base + ['attach-session', '-t', 'graphics'], stdin=slave,
                                      stdout=slave, stderr=slave, env=env, start_new_session=True)
        os.close(slave)
        self.reply = True
        self.delay = delay
        self.answer_on = answer_on
        self.queries = 0
        self.images = {}
        self.last_image = time.monotonic()
        self.done = threading.Event()
        self.error = None
        self.thread = threading.Thread(target=self.drain, daemon=True)
        self.thread.start()

    def drain(self):
        pending = b''
        image_id = None
        payload = None
        replies = []
        try:
            while not self.done.is_set():
                now = time.monotonic()
                for due, response in list(replies):
                    if now >= due:
                        os.write(self.master, response)
                        replies.remove((due, response))
                if not select.select([self.master], [], [], .025)[0]:
                    continue
                data = os.read(self.master, 65536)
                if not data:
                    return
                pending += data
                # The embedded APC header is unchanged by tmux's doubled ESC framing.
                matches = list(re.finditer(rb'\x1b_G([^;\x1b]*);([^\x1b]*)\x1b+\\', pending))
                for match in matches:
                    header = match.group(1)
                    if b'a=q,' in header:
                        self.queries += 1
                        if self.reply and self.queries >= self.answer_on:
                            replies.append((time.monotonic() + self.delay, b'\x1b_Gi=255;OK\x1b\\\x1b[6;16;8t'))
                    if b'a=T,' in header:
                        image_id = int(re.search(rb'(?:^|,)i=(\d+)', header).group(1))
                        payload = hashlib.sha256()
                    if payload is not None and re.search(rb'(?:^|,)m=[01](?:,|$)', header):
                        payload.update(match.group(2))
                        self.last_image = time.monotonic()
                        if re.search(rb'(?:^|,)m=0(?:,|$)', header):
                            self.images[image_id] = payload.hexdigest()
                            payload = None
                if matches:
                    pending = pending[matches[-1].end():]
                pending = pending[-16384:]
        except (OSError, AttributeError, ValueError):
            if not self.done.is_set():
                self.error = 'PTY closed unexpectedly'

    def close(self):
        self.done.set()
        self.thread.join(timeout=1)
        if self.child.poll() is None:
            self.child.terminate()
        try:
            self.child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.child.kill()
            self.child.wait()
        os.close(self.master)


client = None
results = {}
try:
    source = tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'graphics', '-x', '120', '-y', '40',
                  '-P', '-F', '#{pane_id}', 'sleep 120')
    tmux('set-option', '-g', 'allow-passthrough', 'on')
    viewer = tmux('new-window', '-d', '-n', 'viewer', '-P', '-F', '#{pane_id}', '/bin/sh')
    client = Client()
    wait(lambda: tmux('list-clients', '-F', '#{client_pid}').strip(), 'attach client')
    command = shlex.join(['python3', str(root / 'scripts' / 'run_owned.py'), str(root / args.binary),
                          '--demo', '--read-only', '--log', str(trace)])
    # Reproduce typing into a shell in the non-visible window.
    tmux('send-keys', '-t', viewer, '-l', command)
    tmux('send-keys', '-t', viewer, 'Enter')
    wait(lambda: 'event=first_frame' in log(), 'hidden startup')
    time.sleep(1.1)
    hidden = tmux('capture-pane', '-p', '-t', viewer)
    (out / 'hidden.txt').write_text(hidden + '\n')
    assert 'braille:' not in hidden
    assert not any('\u2800' <= c <= '\u28ff' for c in hidden), 'hidden startup rendered braille'
    assert 'waiting for visible window' in hidden
    assert client.queries == 0
    results['hidden_pending'] = True

    tmux('select-window', '-t', viewer)
    wait(lambda: 'event=capabilities graphics=true pending=false' in log(), 'visible graphics reply')
    wait(lambda: len(re.findall(r'event=picture_ready key=@', log())) >= 6
         and len(client.images) == 20 and time.monotonic() - client.last_image > .6, 'initial full upload')
    first_images = dict(client.images)
    first_ids = set(first_images)
    first_queries = client.queries
    assert first_queries == 1
    results['visible_kitty'] = True
    results['initial_image_ids'] = sorted(first_ids)
    (out / 'kitty.txt').write_text(tmux('capture-pane', '-p', '-t', viewer) + '\n')

    # Replace the viewing terminal with a new pid/tty while the same ncm stays alive.
    client.close()
    client = Client()
    wait(lambda: client.queries == 1 and all(client.images.get(i) == digest for i, digest in first_images.items()), 'reattach query and full retransmit')
    assert client.error is None
    results['client_change_full_retransmit'] = sorted(client.images)

    # Once confirmed, a later missed probe must keep images enabled.
    client.reply = False
    client.images.clear()
    before_timeouts = log().count('event=probe_timeout')
    tmux('send-keys', '-t', viewer, 'g')
    wait(lambda: log().count('event=probe_timeout') > before_timeouts, 'confirmed missed probe timeout')
    assert 'braille:' not in tmux('capture-pane', '-p', '-t', viewer)
    wait(lambda: all(client.images.get(i) == digest for i, digest in first_images.items()),
         'confirmed retransmission despite missed probe')
    results['confirmed_missed_probe_stays_kitty'] = True

    client.reply = True
    client.images.clear()
    tmux('send-keys', '-t', viewer, 'g')
    wait(lambda: client.queries >= 3 and all(client.images.get(i) == digest for i, digest in first_images.items()), 'g recovers and retransmits all images')
    wait(lambda: 'braille:' not in tmux('capture-pane', '-p', '-t', viewer), 'clear braille reason')
    results['manual_reprobe_recovered'] = True
    def quit_viewer():
        pid = int(re.search(r'event=start pid=(\d+)', log()).group(1))
        def exited():
            try:
                os.kill(pid, 0)
                return False
            except ProcessLookupError:
                return True
        tmux('send-keys', '-t', viewer, 'q')
        wait(exited, 'quit was reaped by pane-local parent', timeout=3)
    quit_viewer()

    def start_case(name, delay=0, answer_on=1, reply=True):
        global trace
        trace = out / f'{name}.log'
        trace.unlink(missing_ok=True)
        traces.append(trace)
        client.delay = delay
        client.answer_on = answer_on
        client.reply = reply
        client.queries = 0
        client.images.clear()
        command = shlex.join(['python3', str(root / 'scripts' / 'run_owned.py'), str(root / args.binary),
                              '--demo', '--read-only', '--log', str(trace)])
        tmux('send-keys', '-t', viewer, '-l', command)
        tmux('send-keys', '-t', viewer, 'Enter')
        wait(lambda: 'event=first_frame' in log(), f'{name} first placeholder')

    start_case('delayed-600ms', delay=.6)
    tmux('send-keys', '-t', viewer, 'Right')
    wait(lambda: 'event=input_painted selected=1' in log(), 'input painted during pending probe')
    wait(lambda: 'event=capabilities graphics=true pending=false' in log(), '600ms reply confirms kitty')
    rows = log().splitlines()
    painted = next(i for i, row in enumerate(rows) if 'event=input_painted selected=1' in row)
    confirmed = next(i for i, row in enumerate(rows) if 'event=capabilities graphics=true pending=false' in row)
    assert painted < confirmed, 'input/render blocked until reply'
    assert client.queries == 1
    results['delayed_600ms_kitty_and_responsive'] = True
    quit_viewer()

    start_case('third-retry', answer_on=4)
    wait(lambda: 'event=capabilities graphics=true pending=false' in log(), 'third retry confirms kitty', timeout=12)
    assert client.queries == 4, client.queries
    assert log().count('event=probe_timeout') == 3
    results['third_retry_kitty'] = True
    quit_viewer()

    start_case('late-1400ms', delay=1.4)
    wait(lambda: 'event=capabilities graphics=true pending=false' in log(), 'reply after one-second timeout')
    assert client.queries == 1
    assert log().count('event=probe_timeout') == 1
    results['late_reply_after_timeout_kitty'] = True
    quit_viewer()

    start_case('visible-no-reply', reply=False)
    wait(lambda: 'braille: no graphics reply' in tmux('capture-pane', '-p', '-t', viewer),
         'five visible misses settle in braille', timeout=14)
    assert client.queries == 5
    time.sleep(2.2)
    assert client.queries == 5, 'probe retries did not stop'
    braille = tmux('capture-pane', '-p', '-t', viewer)
    assert any('\u2800' <= c <= '\u28ff' for c in braille)
    assert '0.1.0' not in braille
    (out / 'braille.txt').write_text(braille + '\n')
    results['five_visible_misses_braille'] = True
    # A late reply is still accepted from ordinary input even after settling.
    os.write(client.master, b'\x1b_Gi=255;OK\x1b\\\x1b[6;16;8t')
    wait(lambda: len(client.images) == 20, 'late reply after settling restores every image')
    assert 'braille:' not in tmux('capture-pane', '-p', '-t', viewer)
    results['late_reply_after_settling_kitty'] = True
    quit_viewer()
    results['quit_reaped'] = True

finally:
    # The pane-local run_owned parent waits for every ncm before we remove its server.
    for owned_trace in traces:
        reap_from_trace(owned_trace, root)
    if client is not None:
        client.close()
    subprocess.run(base + ['kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    socket.unlink(missing_ok=True)
    (out / 'graphics.json').write_text(json.dumps(results, indent=2) + '\n')
print(json.dumps(results, indent=2))
