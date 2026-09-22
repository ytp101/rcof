# Run P2: local poor-network experiments

P2 keeps the same two native Rust clients and loopback signaling service. A Rust UDP relay now sits between the clients' media endpoints. This is a local learning simulator.

## Watch a native demonstration

```sh
cargo build --locked
python3 scripts/check_p2.py --gui --profile constrained --adaptive --duration 36 --output runs/p2-demo
```

Two windows open with silent tone/pattern sources. This is an interactive demo: leaving a call or closing a window early is normal and is not graded against the requested full duration. Logs and final measurements are still saved. Add `--verify-gui` for unattended full-duration GUI assertions; CLI runs remain strict by default. At second 5 the relay limits **each direction** to 128 kbit/s and adds 30 ms delay with ±10 ms jitter. Six seconds before the requested duration, the relay removes that impairment. The displayed Sending preset updates as the loss-based policy responds. **Interactive windows and calls have no automatic shutdown timer**, even when `--duration` is omitted. In this mode `--duration` controls the impairment schedule only; its final phase stays active until you close both windows or press Ctrl+C. Leave and rejoin both peers to change source settings. Only `--verify-gui` enables timed calls and window closure. Use the normal `python3 scripts/demo_p1.py` launcher for an unrestricted manual call, including microphone/camera use.

The GUI has Tiny and Minimal quality presets, an Adapt video to loss checkbox, a current sending-preset label, and a simulated-network indicator when launched through the lab. Its status changes to Media interrupted after 1.5 seconds without received audio, then returns to connected when audio resumes. Camera selection still starts capture only after both peers connect.

## Reproducible checks

```sh
python3 scripts/check_p2.py --profile baseline --quality tiny --duration 20 --output runs/p2-baseline
python3 scripts/check_p2.py --profile constrained --adaptive --duration 36 --output runs/p2-constrained
python3 scripts/check_p2.py --profile loss --quality low --adaptive --duration 26 --output runs/p2-loss
python3 scripts/check_p2.py --profile outage --quality low --duration 24 --output runs/p2-outage
python3 scripts/check_p2.py --profile setup --quality tiny --duration 20 --output runs/p2-setup
python3 scripts/check_p2.py --profile blackhole --duration 12 --output runs/p2-blackhole
```

| Profile | What happens |
| --- | --- |
| baseline | No impairment; optional `--kbps` cap applies from startup |
| constrained | At second 5: 128 kbit/s, 30±10 ms delay; clear six seconds before call duration |
| loss | At second 5: 384 kbit/s, 40±15 ms delay, 3% independent datagram loss; clear six seconds before duration |
| outage | Drop all media-path datagrams from seconds 7 to 10, then restore |
| setup | From startup: 256 kbit/s, 60±10 ms delay, 1% datagram loss, including ICE/DTLS |
| blackhole | Drop all datagrams from startup; clients must time out without receiving media |

Use durations at least 18 seconds for constrained/loss/outage profiles; blackhole requires no connected media. GUI demos use default adaptive video; the CLI uses adaptation only with `--adaptive`. `--quality` chooses the initial/maximum profile. `--audio-only` applies to CLI baseline measurements.

Each run writes `link-config.json`, `link.json`, `a.json`, `b.json`, and logs. CLI runs also sample process CPU/RSS including the relay, server, and FFmpeg descendants. Generated-source checks run every child under the existing macOS sandbox that blocks external networking. No system network settings or administrator privileges are needed.

## How the relay works

Each client binds a fixed loopback UDP port with `--media-port` and rewrites every remote SDP candidate to `--peer-relay`. The lab binds only 127.0.0.1 and uses two relay sockets so source addresses match the advertised candidates. The same relay carries ICE, DTLS, SRTP, and RTCP. HTTP signaling is separate and unimpaired. The all-drop test proves setup cannot fall back to an alternate direct path; the mid-call blackout test verifies delivery stops and media counters subsequently resume.

There are independent bandwidth serializers and queues in both directions. The simulator includes 28 bytes of IPv4/UDP overhead in its byte budget. Queues default to 32 KiB total per direction, including packets in propagation, and 150 ms maximum pre-transmission residence. Audio and control have priority over queued video; packets already transmitting cannot be preempted. Stale video is discarded rather than accumulating delay. Outages flush queued/in-flight packets.

Jitter is uniform in ±configured milliseconds, clamped to nonnegative propagation delay. Loss is independent per datagram using separate seeded random sequences per direction. Seeds reproduce the impairment decisions for the same arrival sequence; OS scheduling and codec output still make whole-run results vary. The 1 ms scheduler is conservative and can deliver below a nominal cap; it is not a precision network emulator or a congestion-control proof.

For custom phase schedules, edit a saved config and launch `target/debug/rcof link --config PATH --report PATH`, plus the signaling server and clients using its four endpoint ports. Every phase has `at_s`, `kbps` (0 = unlimited), `delay_ms`, `jitter_ms`, `loss_percent`, and `outage`. JSON reports retain the complete configuration and one-second cumulative snapshots (the most recent 3,600 snapshots; total counters cover the whole run). A custom relay `duration_s` of 0 runs until interrupted. Both directions currently use the same phase settings but independent queues and loss sequences.

## Recovery and limits

Video adapts through Standard → Low → Minimal → Tiny after two reporting intervals with at least ~5% loss. Eight near-clean reports allow one upward step, capped at the user's selection. This simple policy can oscillate near capacity; it is not a full bandwidth estimator. Encoder restarts produce keyframes; receivers restart their decoder on resolution changes. Periodic keyframes recover after missing video frames. Default repeated NACK retransmissions were removed because they amplified congestion in the first experiment.

Audio has a 30 ms reorder wait for missing sequences and uses Opus concealment for at most three missing 20 ms frames. Concealment is counted separately from received frames and does not mean lost speech was recovered. Longer silence/outages remain audible. These tests establish packet flow and decoder behavior, not subjective intelligibility.

The tested three-second outage resumes within the existing connection. A failed ICE connection or a disconnection persisting more than 12 seconds ends the call; the user must rejoin after the link is restored. Automatic reconnection after terminal failure, echo cancellation, sophisticated jitter/clock-drift control, and real-network deployment remain future work. See [the bandwidth note](low-bandwidth-video.md) before treating encoder targets as link budgets.

One direct speaker regression showed an intermittent playback overflow; built-in-speaker and default-output repeats passed. The limitation is retained in the [optimization report](../reports/018-bandwidth-optimization.md).
