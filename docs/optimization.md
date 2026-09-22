# Optional bandwidth optimization

The original codec settings remain the default. The new modes trade packetization delay and, in Minimum, speech fidelity for lower traffic. Select **Audio bandwidth** before joining, independently on each peer.

| Mode | Opus target | Packet duration | Packets/second | Silence compression |
| --- | --- | --- | --- | --- |
| Original (`standard`) | 24 kbit/s | 20 ms | 50 | Off |
| Efficient | 24 kbit/s | 40 ms | 25 | On |
| Minimum | 16 kbit/s | 60 ms | ~16.7 | On |

Efficient adds up to 20 ms and Minimum up to 40 ms of packet assembly time compared with Original. Playback also reserves enough samples for the received packet duration, which can increase startup/rebuffer delay. Larger packets mean more sound lost when one packet disappears. The receiver uses the actual peer packet duration and limits concealment to at most 60 ms; long outages do not enqueue a large catch-up burst.

Silence compression uses Opus DTX. Small comfort-noise packets still travel at the configured cadence, maintaining playback, loss tracking, and connection health. Select **Keep quiet audio** or pass `--no-audio-dtx` to disable it for comparison. It reduces payload during quiet periods; it does not eliminate network traffic or lower the bandwidth needed while somebody speaks. CLI `--source silence` is a deterministic test source, not a microphone recording.

**Audio encoding effort** defaults to 10 (the original Opus setting). Lower values reduce encoder work but can affect fidelity. The CLI accepts `--audio-complexity 0..10`; 5 is an optional CPU-saving experiment, not a proven equal-quality setting. This is independent of audio bitrate and packet duration.

**Video refresh interval** remains one second by default. Two seconds can save full-picture refresh traffic, with a longer wait for recovery after loss. CLI `--keyframe-seconds` accepts 1–5 seconds. Tiny still means 160×90 at 5 fps; this change does not improve picture detail.

## Measured results

With generated tone/pattern media, Original audio plus Tiny video averaged 76.74 kbit/s per direction; Efficient averaged 61.62; Minimum with two-second video refresh averaged 45.25—about **41% below Original**. Minimum passed a 64 kbit/s cap with 100/90 video frames over 20 seconds and no audio sequence gaps. A 48 kbit/s cap produced only 10/9 video frames and is not recommended for continuous video. Audio-only decreased from about 54.45 to 25.82 kbit/s. These are local measurements, not a guaranteed speech/camera minimum.

Effort 5 reduced the measured CPU estimate in the DTX-disabled comparison, with essentially unchanged traffic. Start listening at the default effort 10, then compare 5 separately. Full data and the approximate debug/release CPU comparison are in the report below.

## Try it manually

Build an optimized executable:

```sh
cargo build --release --locked --offline
python3 scripts/demo_p1.py --binary target/release/rcof --audio-profile minimum --quality tiny --keyframe-seconds 2
```

Both windows initially use a generated pattern and silent-output test tone. Leave both calls, choose Microphone/Camera and your output device, then join both again. The windows have no automatic shutdown timer. Choose Original to compare against the previous sound, or Efficient to retain the 24 kbit/s audio target.

To test the same settings through a 64 kbit/s cap in each direction:

```sh
python3 scripts/check_p2.py --binary target/release/rcof --gui --quality tiny --audio-profile minimum --keyframe-seconds 2 --kbps 64
```

The cap includes IPv4/UDP and encrypted media/control, but not Wi-Fi or VPN overhead. Real-camera movement and noise can use more traffic than the test pattern.

Package an optimized local app with `sh scripts/package_macos.sh --release`. FFmpeg and Opus remain local prerequisites; this is not a self-contained distribution bundle.

## Reproduce measurements

```sh
python3 scripts/compare_bandwidth.py --binary target/release/rcof --output runs/optimization
```

The script runs cases sequentially: original/efficient/minimum, longer video refreshes, 64/48 kbit/s caps, audio-only, silence, packet loss, and outage recovery. It saves full per-peer and relay results plus `comparison.json`. Steady rates use snapshots from second 4 through three seconds before the requested end. All bandwidth values are per direction. Under a cap, low delivered bandwidth alone is not a win: check received audio samples, video frame counts, losses, and errors.

Constant tones are not speech: the lower-effort encoder may classify them as non-speech and use very little payload with DTX enabled. The suite also repeats the encoding-effort comparison with DTX disabled. Do not advertise tone/silence figures as a guaranteed speech budget.

The original and 64 kbit/s candidate cases must deliver at least 90% of expected audio samples, at least 85% of the expected Tiny video frames, and no audio sequence gaps. The 48 kbit/s case is exploratory. CPU samples are OS estimates for the full server/relay/two-client/FFmpeg process tree, not an isolated benchmark.

Audio `tx_frames`/`rx_frames` continue to count packets. New `tx_audio_samples`/`rx_audio_samples` fields count 48 kHz samples, excluding concealed audio, so checks compare actual audio duration across packet sizes. `audio_packet_ms`, `audio_target_bps`, and `audio_complexity` describe the local sender. Existing frame-to-decode measurements start at packet construction, excluding microphone buffering, packet assembly, and device playback; they are not mouth-to-ear latency measurements.

See [Step 018](../reports/018-bandwidth-optimization.md) for measured results and retained limitations.
