#!/usr/bin/env python3
"""Headless release measurements. Launch this script through the host heavy wrapper.

The viewer runs in a /dev/null-configured private tmux server, and reads the
explicit live socket. All actions below target the private socket exclusively.
Graphics runs inject the kitty/cell-size response into that private viewer's
stdin. This measures rendering/encoding, not a physical terminal's display.
Only timing logs are saved; no live screen or transcript bodies are retained.
"""
import argparse
from process_cleanup import reap_from_trace
import json
import os
from pathlib import Path
import subprocess
import time
import shlex
import signal
import ctypes
import sys

parser = argparse.ArgumentParser()
parser.add_argument('--socket', default='/private/tmp/tmux-501/default')
parser.add_argument('--session', default='main')
parser.add_argument('--seconds', type=int, default=35)
parser.add_argument('--graphics', action='store_true')
parser.add_argument('--demo', action='store_true')
parser.add_argument('--stats', action='store_true')
parser.add_argument('--visibility-smoke', action='store_true')
parser.add_argument('--idle-fixture', action='store_true')
parser.add_argument('--idle-followers', type=int, default=0, help='attach this many synthetic idle transcripts on the private idle server')
parser.add_argument('--width', type=int, default=160)
parser.add_argument('--height', type=int, default=50)
parser.add_argument('--name', default='live')
parser.add_argument('--server-cpu', action='store_true', help='sample live tmux CPU with a 20s without-viewer baseline (macOS)')
parser.add_argument('--profile', action='store_true', help='sample the owned ncm for 10s after startup (macOS)')
args = parser.parse_args()
if args.idle_followers < 0 or (args.idle_followers and not args.idle_fixture):
    parser.error('--idle-followers requires --idle-fixture and a nonnegative count')
root = Path(__file__).resolve().parent.parent
out = root / 'target' / 'ncm-measurements'
out.mkdir(parents=True, exist_ok=True)
socket = out / f'measure-{os.getpid()}.sock'
trace = out / f'{args.name}.log'
trace.unlink(missing_ok=True)
query_log = out / f'{args.name}-queries.log'
query_log.unlink(missing_ok=True)
env = dict(os.environ)
env['NCM_TMUX_QUERY_LOG'] = str(query_log)
env.pop('TMUX', None)
env.pop('TMUX_PANE', None)
env.pop('NCM_TMUX_SOCKET', None)
source_socket = out / f'idle-{os.getpid()}.sock'
viewer_pid = None
load_before = os.getloadavg()
epoch_offset = time.time() - time.clock_gettime(time.CLOCK_MONOTONIC)
server_cpu = None

def native_cpu(pid):
    # PROC_PIDTASKINFO: six uint64 fields followed by twelve int32 fields.
    info = ctypes.create_string_buffer(96)
    libproc = ctypes.CDLL('/usr/lib/libproc.dylib')
    libproc.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]
    if libproc.proc_pidinfo(pid, 4, 0, info, len(info)) != len(info):
        raise RuntimeError(f'libproc taskinfo failed for tmux pid {pid}')
    values = (ctypes.c_uint64 * 6).from_buffer(info)
    return (values[2] + values[3]) / 1e9

def server_snapshot():
    return (time.clock_gettime(time.CLOCK_MONOTONIC), native_cpu(server_cpu['pid']))

def server_percent(start, end):
    return 100 * (end[1] - start[1]) / (end[0] - start[0])

def interrupted(signum, _frame):
    raise SystemExit(f'measurement interrupted by signal {signum}')

signal.signal(signal.SIGTERM, interrupted)

def viewer_alive():
    if viewer_pid is None:
        return False
    try:
        os.kill(viewer_pid, 0)
        return True
    except ProcessLookupError:
        return False

def await_viewer_exit(seconds):
    deadline = time.monotonic() + seconds
    while viewer_alive() and time.monotonic() < deadline:
        time.sleep(.02)
    return not viewer_alive()

def stop_viewer():
    # Only this run's exec'd pane PID; never signal other viewers or the live server.
    for sig in (signal.SIGTERM, signal.SIGKILL):
        if not viewer_alive():
            return
        try:
            os.kill(viewer_pid, sig)
        except ProcessLookupError:
            return
        if await_viewer_exit(2):
            return
    raise RuntimeError(f'measurement viewer {viewer_pid} was not reaped')

def tmux(*command):
    return subprocess.check_output(['tmux', '-S', str(socket), *command], env=env, stderr=subprocess.PIPE).decode().strip()

command = ['python3', str(root / 'scripts' / 'run_owned.py'), str(root / 'target' / 'release' / 'ncm'), '--read-only', '--log', str(trace)]
if args.stats:
    command.append('--stats')
if args.demo:
    command.append('--demo')
elif args.visibility_smoke:
    command += ['--session', 'ncm-measure']
else:
    command += ['--socket', str(source_socket) if args.idle_fixture else args.socket, '--session', 'idle' if args.idle_fixture else args.session]
try:
    if args.server_cpu:
        if sys.platform != 'darwin':
            raise RuntimeError('--server-cpu requires macOS libproc')
        pid = int(subprocess.check_output(['tmux', '-S', args.socket, 'display-message', '-p', '#{pid}'], env=env))
        server_cpu = {'pid': pid, 'baseline_seconds': 20, 'viewer_reads_live_server': not args.idle_fixture}
        before = server_snapshot()
        time.sleep(20)
        server_cpu['without_viewer_pct'] = server_percent(before, server_snapshot())
    if args.idle_fixture:
        source = ['tmux', '-S', str(source_socket)]
        subprocess.check_call(source + ['-f', '/dev/null', 'new-session', '-d', '-s', 'idle', '-x', '255', '-y', '60', 'sleep 120'], env=env)
        for i in range(max(6, args.idle_followers) - 1):
            subprocess.check_call(source + ['new-window', '-d', '-n', f'idle-{i}', 'sleep 120'], env=env)
        if args.idle_followers:
            claude = out / f'{args.name}-fixture' / 'claude'
            (claude / 'sessions').mkdir(parents=True, exist_ok=True)
            project = claude / 'projects' / 'idle'
            project.mkdir(parents=True, exist_ok=True)
            panes = subprocess.check_output(source + ['list-panes', '-s', '-t', 'idle', '-F', '#{pane_id} #{pane_pid} #{window_id}'], env=env).decode().splitlines()
            for i, row in enumerate(panes[:args.idle_followers]):
                pane_id, pane_pid, window_id = row.split()
                session_id = f'idle-{i}'
                (claude / 'sessions' / f'{pane_pid}.json').write_text(json.dumps({'pid': int(pane_pid), 'sessionId': session_id, 'startedAt': int(time.time() * 1000), 'tmux': f'idle:{window_id}.{pane_id}'}))
                records = [
                    {'type': 'user', 'uuid': 'u', 'origin': {'kind': 'human'}, 'message': {'content': 'idle fixture'}},
                    {'type': 'assistant', 'parentUuid': 'u', 'message': {'model': 'idle-fixture', 'usage': {'input_tokens': 42}, 'content': [{'type': 'text', 'text': 'complete'}]}},
                ]
                (project / f'{session_id}.jsonl').write_text(''.join(json.dumps(r) + '\n' for r in records))
            # The viewer server inherits this isolated root; live ~/.claude is untouched.
            env['NCM_CLAUDE_ROOT'] = str(claude)
        # Creates only private idle windows; samples after 65 s avoid second-by-second age labels.
        time.sleep(.1)
    tmux('-f', '/dev/null', 'new-session', '-d', '-s', 'ncm-measure', '-x', str(args.width), '-y', str(args.height), 'sleep 120')
    tmux('set-option', '-g', 'allow-passthrough', 'on' if args.graphics else 'off')
    pane, pid = tmux('new-window', '-d', '-P', '-F', '#{pane_id} #{pane_pid}', '-n', 'viewer', 'exec ' + shlex.join(command)).split()
    # pane_pid is the reaping owner; the trace identifies its ncm child.
    viewer_pid = int(pid)
    if not args.visibility_smoke:
        tmux('select-window', '-t', pane)
    if args.graphics:
        # Start trace is written before terminal setup. Wait briefly for the raw probe.
        deadline = time.monotonic() + 2
        while (not trace.exists() or 'event=probe_start' not in trace.read_text()) and time.monotonic() < deadline:
            time.sleep(.002)
        time.sleep(.015)
        reply = b'\x1b_Gi=255;OK\x1b\\\x1b[6;16;8t'
        tmux('send-keys', '-t', pane, '-H', *[f'{byte:02x}' for byte in reply])
        deadline = time.monotonic() + 2
        while 'event=capabilities' not in trace.read_text() and time.monotonic() < deadline:
            time.sleep(.005)
        if 'event=capabilities graphics=true' not in trace.read_text():
            raise RuntimeError('graphics probe was not accepted; no graphics measurement recorded')
    if args.visibility_smoke:
        def image_bytes():
            return sum(int(dict(token.split('=', 1) for token in row.split())['bytes']) for row in trace.read_text().splitlines() if 'event=image_flush' in row)
        def await_images(previous):
            deadline = time.monotonic() + 5
            while image_bytes() <= previous and time.monotonic() < deadline:
                time.sleep(.05)
            if image_bytes() <= previous:
                raise RuntimeError('viewer did not retransmit on return')
        time.sleep(.5)
        if 'event=capabilities graphics=true' not in trace.read_text():
            raise RuntimeError('emulated graphics reply was not accepted')
        if image_bytes() != 0:
            raise RuntimeError('hidden viewer uploaded images')
        window = tmux('display-message', '-p', '-t', pane, '#{window_id}')
        tmux('select-window', '-t', window)
        await_images(0)
        tmux('select-window', '-t', 'ncm-measure:0')
        time.sleep(.4)
        before = image_bytes()
        time.sleep(.4)
        if image_bytes() != before:
            raise RuntimeError('hidden viewer kept uploading')
        tmux('select-window', '-t', window)
        await_images(before)
        print('private hide/return image retransmission OK')
    else:
        if server_cpu and args.seconds > 15:
            time.sleep(15)
            before = server_snapshot()
            time.sleep(args.seconds - 15)
            server_cpu['with_viewer_pct'] = server_percent(before, server_snapshot())
            server_cpu['with_viewer_seconds'] = args.seconds - 15
        elif args.profile:
            deadline = time.monotonic() + args.seconds
            time.sleep(min(15, args.seconds))
            start = next(row for row in trace.read_text().splitlines() if 'event=start ' in row)
            profile_pid = dict(token.split('=', 1) for token in start.split())['pid']
            subprocess.run(['sample', profile_pid, '10', '-file', str(out / f'{args.name}.sample.txt')], check=True)
            time.sleep(max(0, deadline - time.monotonic()))
        else:
            time.sleep(args.seconds)
    if args.stats:
        screen = tmux('capture-pane', '-p', '-t', pane)
        for needle in ('CPU', 'MB', 'KB/s', 'cap/s'):
            if needle not in screen:
                raise RuntimeError(f'stats dock missing {needle}')
    tmux('send-keys', '-t', pane, '-l', 'q')
    if not await_viewer_exit(2):
        stop_viewer()
    rows = trace.read_text().splitlines()
    samples = []
    catching_up = not args.demo
    followed = 0
    for row in rows:
        if 'event=facts' in row and 'catching_up=' in row:
            facts = dict(token.split('=', 1) for token in row.split())
            catching_up = facts['catching_up'] == 'true'
            followed = int(facts['followed'])
        if 'event=sample' in row:
            fields = dict(token.split('=', 1) for token in row.split())
            sample = {key: float(value) for key, value in fields.items() if key not in ('event',)}
            sample['catching_up'] = catching_up
            samples.append(sample)
    steady = [s for s in samples if s['t_ms'] >= (65000 if args.idle_fixture else 15000) and not s['catching_up']]
    summary = {'name': args.name, 'graphics_response_emulated': args.graphics, 'geometry': [args.width, args.height], 'samples': len(steady), 'followed_transcripts': followed, 'first_frame': next((r for r in rows if 'event=first_frame' in r), None)}
    summary['steady_seconds'] = sum(s['interval_s'] for s in steady)
    summary['cpu_mean_method'] = 'interval_weighted'
    summary['load_average_before'] = load_before
    summary['load_average_after'] = os.getloadavg()
    summary['poll_errors'] = sum('event=poll_error' in row for row in rows)
    if server_cpu:
        summary['live_tmux_server_cpu'] = server_cpu
    if samples:
        summary['whole_run_peak_rss_mib'] = max(s.get('peak_rss_mb', s['rss_mb']) for s in samples)
        startup = [s for s in samples if s['t_ms'] < 15000 or s['catching_up']]
        if startup:
            summary['startup_peak_rss_mib'] = max(s.get('peak_rss_mb', s['rss_mb']) for s in startup)
    for key in ('cpu_pct', 'child_cpu_pct', 'rss_mb', 'image_kbs', 'captures_s', 'changed', 'writes'):
        values = [s[key] for s in steady]
        if values:
            mean = sum(s[key] * s['interval_s'] for s in steady) / summary['steady_seconds'] if key in ('cpu_pct', 'child_cpu_pct', 'image_kbs', 'captures_s') else sum(values) / len(values)
            summary[key] = {'mean': mean, 'max': max(values)}
    if steady:
        combined = [s['cpu_pct'] + s['child_cpu_pct'] for s in steady]
        summary['combined_cpu_pct'] = {'mean': summary['cpu_pct']['mean'] + summary['child_cpu_pct']['mean'], 'max': max(combined)}
        invocations = [float(line.split()[0]) for line in query_log.read_text().splitlines()]
        count = sum(sum(epoch_offset + s['mono_ns'] / 1e9 - s['interval_s'] <= stamp < epoch_offset + s['mono_ns'] / 1e9 for stamp in invocations) for s in steady)
        summary['spawns_s'] = count / summary['steady_seconds']
    (out / f'{args.name}.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(summary, indent=2))
finally:
    reap_from_trace(trace,root)
    try:
        stop_viewer()
    finally:
        try:
            tmux('kill-server')
        except subprocess.CalledProcessError:
            pass
        socket.unlink(missing_ok=True)
        if args.idle_fixture:
            subprocess.run(['tmux', '-S', str(source_socket), 'kill-server'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            source_socket.unlink(missing_ok=True)
