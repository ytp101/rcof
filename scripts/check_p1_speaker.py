#!/usr/bin/env python3
"""Short audible tone playback check with simultaneous VP8; no microphone capture."""
import argparse, json, signal, socket, subprocess, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.request import Request, ProxyHandler, build_opener
from urllib.error import HTTPError
from pathlib import Path
root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser()
parser.add_argument('--duration', type=int, default=20)
parser.add_argument('--output-device')
parser.add_argument('--binary', type=Path, default=root / 'target/debug/rcof')
parser.add_argument('--poll-delay-ms', type=int, default=0,
                    help='Delay signaling responses only; media still uses normal loopback')
parser.add_argument('--output', type=Path, default=root / 'runs/p1-speaker-final')
parser.add_argument('--audio-profile',choices=['standard','efficient','minimum'],default='standard')
parser.add_argument('--audio-only',action='store_true')
parser.add_argument('--audio-complexity',type=int,choices=range(11),default=10)
parser.add_argument('--no-audio-dtx',action='store_true')
args = parser.parse_args()
out = args.output
out.mkdir(parents=True, exist_ok=True)
with socket.socket() as sock:
    sock.bind(('127.0.0.1', 0))
    address = f'127.0.0.1:{sock.getsockname()[1]}'
binary = str(args.binary.resolve())
server = subprocess.Popen([binary, 'server', '--listen', address])
children = []
proxy = None
if args.poll_delay_ms:
    upstream = address
    class DelayedSignaling(BaseHTTPRequestHandler):
        def forward(self):
            body = self.rfile.read(int(self.headers.get('Content-Length', '0'))) or None
            request = Request('http://' + upstream + self.path, data=body, method=self.command,
                              headers={'Content-Type': 'application/json'})
            opener = build_opener(ProxyHandler({}))
            try:
                response = opener.open(request, timeout=5)
            except HTTPError as error:
                response = error
            with response:
                payload = response.read()
                status = response.code
            if self.command == 'GET' and self.path.startswith('/session/'):
                time.sleep(args.poll_delay_ms / 1000)
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        do_GET = do_POST = do_DELETE = forward
        def log_message(self, *_):
            pass
    proxy = ThreadingHTTPServer(('127.0.0.1', 0), DelayedSignaling)
    threading.Thread(target=proxy.serve_forever, daemon=True).start()
    address = f'127.0.0.1:{proxy.server_port}'

try:
    import time
    time.sleep(.5)
    for name in ['a', 'b']:
        with (out / (name + '.log')).open('w') as log:
            command = [binary, 'call', '--id', name, '--server', address,
                       '--audio-complexity',str(args.audio_complexity),'--audio-profile',args.audio_profile,'--video', 'off' if args.audio_only else 'pattern', '--duration', str(args.duration), '--no-stdin',
                       '--stats-file', str(out / (name + '.json'))]
            if args.no_audio_dtx:command+=['--no-audio-dtx']
            if name == 'b':
                command += ['--sink', 'speaker']
                if args.output_device:
                    command += ['--output-device', args.output_device]
            children.append(subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT))
    for child in children:
        assert child.wait(timeout=args.duration + 30) == 0
    data = json.loads((out / 'b.json').read_text())
    keys = ['rx_frames', 'video_rx_frames', 'playback_missing_samples',
            'playback_dropped_samples', 'audio_late_samples', 'device_errors']
    print({key: data[key] for key in keys})
    assert data['rx_audio_samples'] > args.duration * 45120 and (data['video_rx_frames'] == 0 if args.audio_only else data['video_rx_frames'] > args.duration * 10)
    assert data['device_errors'] == 0
    assert data['playback_dropped_samples'] == 0, 'A/V playback buffer overflow'
    assert data['playback_missing_samples'] == 0, 'Audio underrun during the local baseline'
    assert data['audio_late_samples'] == 0, 'Audio missed its deadline by over 20 ms'
finally:
    for child in children:
        if child.poll() is None:
            child.terminate()
            child.wait(timeout=5)
    if proxy:
        proxy.shutdown()
        proxy.server_close()
    server.send_signal(signal.SIGINT)
    server.wait(timeout=5)
