#!/usr/bin/env python3
"""Exercise real server + two native clients. Python standard library only."""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import time
import urllib.request
import urllib.error

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--duration', type=int, default=10)
p.add_argument('--offline', action='store_true', help='macOS sandbox: deny non-loopback networking')
p.add_argument('--output', type=Path, default=ROOT / 'runs' / 'p0-check')
p.add_argument('--binary', type=Path, default=ROOT / 'target' / 'debug' / 'rcof')
p.add_argument('--audio-profile', choices=['standard','efficient','minimum'], default='standard')
args = p.parse_args()
packet_fps = 1000 / {'standard':20,'efficient':40,'minimum':60}[args.audio_profile]
assert args.duration >= 2
args.output.mkdir(parents=True, exist_ok=True)
with socket.socket() as s:
    s.bind(('127.0.0.1', 0))
    port = s.getsockname()[1]
address = f'127.0.0.1:{port}'
base = f'http://{address}'
prefix = ['/usr/bin/sandbox-exec', '-f', str(ROOT / 'scripts/loopback-only.sb')] if args.offline else []
processes = []
files = []
http = urllib.request.build_opener(urllib.request.ProxyHandler({}))

def request(path, payload=None, method=None):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(base + path, data=data, method=method, headers={'Content-Type':'application/json'})
    with http.open(req, timeout=2) as response:
        body = response.read()
        return json.loads(body) if body and body.startswith(b'{') else body

def spawn(name, command, stdin=False):
    log = (args.output / f'{name}.log').open('w')
    files.append(log)
    child = subprocess.Popen(prefix + [str(args.binary)] + command, stdin=subprocess.PIPE if stdin else subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT, text=True)
    processes.append(child)
    return child

def wait_for(predicate, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            if predicate():
                return
        except (urllib.error.URLError, ConnectionError):
            pass
        time.sleep(0.1)
    raise AssertionError('condition timed out; inspect logs in ' + str(args.output))

def client(name, room, duration=0, extra=(), stdin=False):
    return spawn(name, ['call', '--audio-profile', args.audio_profile, '--id', name, '--room', room, '--server', address, '--duration', str(duration), '--stats-file', str(args.output / f'{name}.json')] + ([] if stdin else ['--no-stdin']) + list(extra), stdin)

def command(child, text):
    child.stdin.write(text + '\n')
    child.stdin.flush()

def summary(name):
    return json.loads((args.output / f'{name}.json').read_text())

try:
    server = spawn('server', ['server', '--listen', address])
    wait_for(lambda: request('/health'))
    # Room isolation, duplicate ID, capacity, departure, and expired-token handling.
    first = request('/join', {'room':'limits','id':'one'})['token']
    for ident, expected in [('one',409), ('two',None), ('three',409)]:
        try:
            result = request('/join', {'room':'limits','id':ident})
            assert expected is None
            second = result['token']
        except urllib.error.HTTPError as error:
            assert error.code == expected
    assert request('/session/' + first)['peer'] == 'two'
    request('/session/' + first, method='DELETE')
    assert request('/session/' + second)['peer'] is None
    request('/session/' + second, method='DELETE')

    a = client('a', 'audio', args.duration, extra=['--tone-hz','440'])
    b = client('b', 'audio', args.duration, extra=['--tone-hz','660'])
    # Collect actual RSS/CPU samples while both clients run, not a benchmark promise.
    usage = []
    deadline = time.monotonic() + args.duration + 20
    while a.poll() is None or b.poll() is None:
        assert time.monotonic() < deadline, 'audio run did not finish'
        sample = subprocess.run(['ps','-o','pid=,%cpu=,rss=','-p',f'{a.pid},{b.pid}'],capture_output=True,text=True)
        usage.append({'monotonic':time.monotonic(),'ps':sample.stdout.strip()})
        time.sleep(0.5)
    (args.output / 'resources.json').write_text(json.dumps(usage, indent=2))
    assert a.returncode == b.returncode == 0, 'audio process failed; inspect logs'
    for ident in ['a','b']:
        data = summary(ident)
        assert data['tx_frames'] >= args.duration * packet_fps * .8, data
        assert data['rx_frames'] >= args.duration * packet_fps * .8, data
        assert data['rx_non_silent_frames'] >= args.duration * packet_fps * .7, data
        assert data['decode_errors'] == data['device_errors'] == 0, data
        assert data['frame_to_decode']['samples'] >= args.duration * packet_fps * .8, data
        assert data['sequence_gaps'] == data['late_packets'] == 0, data
        assert data['transport_tx_bytes'] > data['tx_opus_bytes'], data
        assert data['transport_rx_bytes'] > data['rx_opus_bytes'], data
        assert -30 < data['source_rms_dbfs'] < -20, data
    print(f'PASS: {args.duration}s two-way Opus audio, decode and timing telemetry', flush=True)

    # Exercise controls and clean peer leave. Stats let us see silence while muted.
    c = client('c','controls',stdin=True)
    d = client('d','controls',stdin=True)
    wait_for(lambda: 'connection: connected' in (args.output/'c.log').read_text())
    wait_for(lambda: 'connection: connected' in (args.output/'d.log').read_text())
    time.sleep(1)
    command(c,'mute')
    time.sleep(1)
    command(d,'stats')
    time.sleep(0.2)
    def recent_stats():
        rows = (args.output/'d.log').read_text().splitlines()
        return [json.loads(line) for line in rows if line.startswith('{"id"')][-1]
    before = recent_stats()
    time.sleep(1)
    command(d,'stats')
    time.sleep(0.2)
    after = recent_stats()
    assert after['rx_frames'] > before['rx_frames'] + packet_fps * .6
    assert after['rx_non_silent_frames'] == before['rx_non_silent_frames'], 'mute did not silence audio'
    command(c,'unmute')
    time.sleep(1)
    command(d,'stats')
    time.sleep(0.2)
    assert recent_stats()['rx_non_silent_frames'] > after['rx_non_silent_frames'] + packet_fps * .4
    command(c,'quit')
    assert c.wait(timeout=5) == 0
    assert d.wait(timeout=5) == 0
    assert summary('d')['outcome'] == 'peer left'
    print('PASS: mute/unmute, stats, quit, and peer-leave cleanup', flush=True)

    # A lone client must time out and unregister rather than hang.
    lone = client('lone','timeout',extra=['--connect-timeout','1'])
    assert lone.wait(timeout=6) != 0
    assert 'connection timeout' in summary('lone')['outcome']
    token = request('/join', {'room':'timeout','id':'lone'})['token']
    request('/session/' + token, method='DELETE')
    print('PASS: timeout cleanup and same-ID rejoin', flush=True)
    # A killed process cannot send leave; server leases must eventually release it.
    e = client('e','crash')
    f = client('f','crash')
    wait_for(lambda: 'connection: connected' in (args.output/'f.log').read_text())
    crashed_at = time.monotonic()
    e.kill()
    e.wait(timeout=5)
    assert f.wait(timeout=16) in (0, 1)
    assert summary('f')['outcome'] in (
        'peer left', 'error: peer disconnected for more than three seconds',
        'error: media connection failed',
    )
    time.sleep(max(0, 10.5 - (time.monotonic() - crashed_at)))
    token = request('/join', {'room':'crash','id':'e'})['token']
    assert request('/session/' + token)['peer'] is None
    request('/session/' + token, method='DELETE')
    print('PASS: crashed-peer cleanup and lease expiry', flush=True)

    # Loss of signaling must produce errors and release clients, not hang them.
    g = client('g','server-loss')
    h = client('h','server-loss')
    wait_for(lambda: 'connection: connected' in (args.output/'g.log').read_text())
    server.kill()
    server.wait(timeout=5)
    assert g.wait(timeout=10) != 0
    assert h.wait(timeout=10) != 0
    assert summary('g')['outcome'].startswith('error:')
    print('PASS: signaling-server failure cleanup', flush=True)
    print('ALL CHECKS PASSED; artifacts: ' + str(args.output), flush=True)
finally:
    for child in reversed(processes):
        if child.poll() is None:
            child.send_signal(signal.SIGINT)
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait(timeout=5)
    for log in files:
        log.close()
