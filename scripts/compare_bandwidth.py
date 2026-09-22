#!/usr/bin/env python3
"""Sequential local codec/link experiments; no concurrent cases distort CPU samples."""
import argparse
import json
from pathlib import Path
import statistics
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--binary', type=Path, default=ROOT/'target/release/rcof')
p.add_argument('--output', type=Path, default=ROOT/'runs/optimization')
p.add_argument('--duration', type=int, default=20)
p.add_argument('--cases', nargs='+', help='Run only the named cases')
args = p.parse_args()
if args.duration < 18:
    p.error('duration must be at least 18 seconds')
args.output.mkdir(parents=True, exist_ok=True)
cases = [
    ('original', []),
    ('efficient', ['--audio-profile', 'efficient']),
    ('minimum', ['--audio-profile', 'minimum']),
    ('minimum-refresh2', ['--audio-profile', 'minimum', '--keyframe-seconds', '2']),
    ('minimum-64', ['--audio-profile', 'minimum', '--keyframe-seconds', '2', '--kbps', '64']),
    ('minimum-fast', ['--audio-profile', 'minimum', '--audio-complexity', '5', '--keyframe-seconds', '2']),
    ('minimum-fast-64', ['--audio-profile', 'minimum', '--audio-complexity', '5', '--keyframe-seconds', '2', '--kbps', '64']),
    ('minimum-48', ['--audio-profile', 'minimum', '--keyframe-seconds', '2', '--kbps', '48']),
    ('audio-original', ['--audio-only']),
    ('audio-minimum', ['--audio-only', '--audio-profile', 'minimum']),
    ('silence-original', ['--audio-only', '--source', 'silence']),
    ('silence-minimum', ['--audio-only', '--source', 'silence', '--audio-profile', 'minimum']),
    ('minimum-no-dtx', ['--audio-profile', 'minimum', '--keyframe-seconds', '2', '--no-audio-dtx']),
    ('minimum-fast-no-dtx', ['--audio-profile', 'minimum', '--audio-complexity', '5', '--keyframe-seconds', '2', '--no-audio-dtx']),
    ('minimum-fast-no-dtx-64', ['--audio-profile', 'minimum', '--audio-complexity', '5', '--keyframe-seconds', '2', '--no-audio-dtx', '--kbps', '64']),
    ('minimum-loss', ['--audio-profile', 'minimum', '--keyframe-seconds', '2', '--profile', 'loss']),
    ('minimum-outage', ['--audio-profile', 'minimum', '--keyframe-seconds', '2', '--profile', 'outage']),
]
if args.cases:
    unknown = set(args.cases)-{name for name,_ in cases}
    if unknown:p.error('Unknown cases: '+', '.join(sorted(unknown)))
    cases = [(name,extra) for name,extra in cases if name in args.cases]
results = []
for name, extra in cases:
    out = args.output/name
    command = [sys.executable, str(ROOT/'scripts/check_p2.py'), '--binary', str(args.binary),
               '--quality', 'tiny', '--duration', str(args.duration), '--output', str(out), *extra]
    print('RUN', name, flush=True)
    out.mkdir(parents=True, exist_ok=True)
    with (out/'check.log').open('w') as log:
        completed = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT)
    row = {'case': name, 'command': command, 'check_exit': completed.returncode}
    if completed.returncode:
        print((out/'check.log').read_text()[-2500:], flush=True)
    if (out/'link.json').exists():
        link = json.loads((out/'link.json').read_text())
        steady = [s for s in link['snapshots'] if 4 <= s['elapsed_s'] <= args.duration-3]
        if not steady or not all((out/(peer+'.json')).exists() for peer in ['a','b']):
            row['evidence_error'] = 'Missing steady snapshots or completed peer summaries'
            row['check_exit'] = row['check_exit'] or 1
            results.append(row)
            (args.output/'comparison.json').write_text(json.dumps(results,indent=2))
            continue
        for direction in ['a_to_b','b_to_a']:
            first,last = steady[0],steady[-1]
            row[direction+'_steady_kbps'] = (last[direction]['delivered_ip_bytes']-first[direction]['delivered_ip_bytes'])*8/1000/(last['elapsed_s']-first['elapsed_s'])
        for peer in ['a','b']:
            d = json.loads((out/(peer+'.json')).read_text())
            row[peer] = {key:d[key] for key in ['elapsed_seconds','rx_audio_samples','rx_frames','video_rx_frames',
                'sequence_gaps','decode_errors','device_errors','audio_concealed_frames','audio_packet_ms','audio_target_bps','encode','outcome']}
        resources = json.loads((out/'resources.json').read_text())
        # ps %CPU is an OS recent/lifetime estimate, not an isolated CPU benchmark.
        samples = resources[4:-3]
        if samples:
            row['process_tree_median_cpu_percent'] = statistics.median(sum(float(r[2]) for r in sample) for sample in samples)
            row['process_tree_peak_rss_mib'] = max(sum(int(r[3]) for r in sample)/1024 for sample in samples)
    results.append(row)
    (args.output/'comparison.json').write_text(json.dumps(results,indent=2))
    print(json.dumps(row), flush=True)
if any(row['check_exit'] for row in results):
    raise SystemExit('One or more transport checks failed; inspect comparison.json and case logs.')
# Delivery criteria beyond merely receiving one video packet.
for row in results:
    if row['case'] in ['original','efficient','minimum','minimum-refresh2','minimum-64','minimum-fast','minimum-fast-64','minimum-no-dtx','minimum-fast-no-dtx','minimum-fast-no-dtx-64']:
        for peer in ['a','b']:
            assert row[peer]['rx_audio_samples'] >= args.duration*48000*.9, row
            assert row[peer]['video_rx_frames'] >= args.duration*5*.85, row
            assert row[peer]['sequence_gaps'] == 0, row
print('PASS: baseline and 64 kbps delivery criteria. 48 kbps is exploratory; compare video frame counts.', flush=True)
