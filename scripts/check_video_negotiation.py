#!/usr/bin/env python3
"""Exercise mixed outgoing-video settings in both SDP offer directions."""
import json
import signal
import socket
import subprocess
import time
from pathlib import Path
from urllib.request import ProxyHandler, build_opener

root = Path(__file__).resolve().parents[1]
out = root / 'runs/video-negotiation'
out.mkdir(parents=True, exist_ok=True)
binary = str(root / 'target/debug/rcof')
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    address = f'127.0.0.1:{sock.getsockname()[1]}'
prefix = ['/usr/bin/sandbox-exec', '-f', str(root / 'scripts/loopback-only.sb')]
server = subprocess.Popen(prefix + [binary, 'server', '--listen', address], stdout=subprocess.DEVNULL)
children = []
try:
    http = build_opener(ProxyHandler({}))
    for attempt in range(50):
        try:
            with http.open('http://' + address + '/health', timeout=1):
                break
        except OSError:
            time.sleep(.1)
    else:
        raise AssertionError('server did not start')
    for left, right in [('off', 'pattern'), ('pattern', 'off'), ('off', 'off')]:
        room = f'{left}-{right}'
        pair = []
        for name, source in [('a', left), ('b', right)]:
            with (out / f'{room}-{name}.log').open('w') as log:
                child = subprocess.Popen(prefix + [binary, 'call', '--id', name,
                    '--room', room, '--server', address, '--video', source,
                    '--duration', '8', '--no-stdin', '--stats-file',
                    str(out / f'{room}-{name}.json')], stdout=log, stderr=subprocess.STDOUT)
            children.append(child)
            pair.append(child)
        for child in pair:
            assert child.wait(timeout=25) == 0, f'{room}: client failed'
        for name, source, remote in [('a', left, right), ('b', right, left)]:
            data = json.loads((out / f'{room}-{name}.json').read_text())
            assert data['rx_frames'] > 250, data
            assert data['decode_errors'] == 0, data
            if remote == 'pattern':
                assert data['video_rx_frames'] > 60, data
            else:
                assert data['video_rx_frames'] == 0, data
            if source == 'off':
                assert data['video_tx_frames'] == 0, data
        print(f'PASS: {left} offers to {right}; audio and expected video flow', flush=True)
finally:
    for child in children:
        if child.poll() is None:
            child.terminate()
            child.wait(timeout=5)
    server.send_signal(signal.SIGINT)
    server.wait(timeout=5)
