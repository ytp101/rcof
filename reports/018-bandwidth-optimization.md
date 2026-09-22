# Step 018 — Optional bandwidth and CPU optimization

The user manually tried the original app and reported clear audio and an understandable low-detail picture. This step preserves that codec configuration and adds experimental alternatives for a direct listening comparison.

## Implementation

- Original: 24 kbit/s mono Opus, 20 ms packets, no DTX. Efficient: 24 kbit/s, 40 ms packets, DTX. Minimum: 16 kbit/s, 60 ms packets, DTX.
- The receiver reads duration from each incoming Opus packet, so peers can select different profiles. RTP timestamps advance by the actual number of samples. Loss concealment covers no more than 60 ms, using the previous received packet duration.
- Silence uses Opus DTX while preserving small packets at the normal cadence. This avoids treating intentional silence as a broken link or missing audio, without introducing a separate comfort-noise scheduler.
- Audio startup/rebuffer reserve now accounts for the incoming packet size, independent of the local sender profile. Output queues remain bounded: 200 ms audio-only, 240 ms with scheduled video. A development audio-only check initially found 128 missing samples with the old reserve; the fix passed subsequent audio-only and audio/video speaker checks with zero missing/dropped/late samples.
- Opus encoding effort is adjustable from 0 to 10; the original default is 10. Lower effort may trade fidelity for reduced CPU work. It does not change packet duration or the nominal bitrate.
- Video keyframe interval is configurable from 1–5 seconds, default 1. Longer intervals can reduce full-frame refresh traffic but increase visible recovery time after lost data. No resolution or frame-rate change is hidden in this setting.
- New sample counters support fair audio-delivery checks across packet durations. Existing frame counters still count packets.
- CLI, native UI, manual launcher, lab scripts, and local app packaging support the new choices. Optimized release builds are available; no publishing or remote deployment was performed.

## Measurement method

Two real local clients exchange generated tone/pattern media through the existing encrypted loopback UDP relay. Each case runs sequentially for 20 connected seconds. Steady bandwidth is computed from relay cumulative byte snapshots from second 4 to three seconds before the scheduled end, per direction, including IPv4/UDP overhead and encrypted media/control. Link-layer and VPN overhead are excluded.

The full process-tree CPU figures come from `ps`, include both peers, FFmpeg children, the server and relay, and are approximate OS estimates. They are not CPU usage for a single GUI, and they are not a controlled hardware benchmark. RSS sums are process RSS, not unique physical memory.

## Measured bandwidth

Steady IPv4/UDP kbit/s per direction, averaging the two directions for readability:

| Configuration | kbit/s | Change versus relevant original |
| --- | ---: | ---: |
| Original audio + Tiny video | 76.74 | Baseline |
| Efficient audio + Tiny video | 61.62 | 19.7% lower |
| Minimum audio + Tiny video, 1 s refresh | 47.99 | 37.5% lower |
| Minimum audio + Tiny video, 2 s refresh | 45.25 | 41.0% lower |
| Original audio only, tone | 54.45 | Baseline |
| Minimum audio only, tone | 25.82 | 52.6% lower |
| Original audio only, silence | 40.05 | Baseline silence |
| Minimum audio only, silence | 11.48 | 71.3% lower during silence |

The unconstrained video cases received 101 frames per peer in about 20 seconds, with zero audio gaps or decoder/device errors. The 64 kbit/s cap with Minimum + two-second refresh delivered 100/90 video frames, zero audio gaps, and 961,920 audio samples per peer. This improves the demonstrated link budget from the previous 96 to **64 kbit/s per direction (33.3% lower)**. It is not a guaranteed real-camera minimum.

At 48 kbit/s, only 10/9 video frames arrived over 20 seconds, and one peer lost three audio packets. This remains unsuitable for continuous video despite a lower delivered traffic counter.

### Avoiding a misleading result

The lower-effort encoder (complexity 5) with DTX produced only about 31 kbit/s total with the stationary tone, versus 45 kbit/s at effort 10. Its encoded audio payload dropped from about 14.8 to 1.4 kbit/s even though the source was not silent. This is content-dependent codec/DTX behavior, not evidence that ordinary speech needs so little bandwidth. That 31 kbit/s number is excluded from the headline recommendation. The follow-up effort comparison disables DTX to separate encoder work from silence suppression.

### Loss and outage

With 3% configured datagram loss during the impaired phase, the peers received 98/92 video frames; one concealed five missing audio packets, with zero decode errors. The three-second all-drop outage lost 50 audio packets per direction (60 ms each), concealed only one packet rather than replaying a three-second backlog, and resumed audio/video. Both peers received 84 video frames over the full outage run.

Raw evidence: [comparison](../docs/benchmarks/optimization/comparison.json), [run metadata](../docs/benchmarks/optimization/metadata.json), and per-case summaries/relay reports in the same directory.


## Encoder effort and resource use

With DTX disabled, Minimum + two-second video refresh used 45.25 kbit/s at effort 10 and 45.05 kbit/s at effort 5. Both received 101 video frames per peer, with no audio gaps. The measured process-tree median CPU estimate decreased from 5.8% to 4.3%, and the audio encode p95 histogram upper bound decreased from 2,047 to 1,023 microseconds. This is a useful optional CPU tradeoff, not evidence of equal speech fidelity. RSS stayed near 125 MiB for the full headless lab tree.

Effort 5 with DTX disabled also passed the 64 kbit/s cap: 100/90 received video frames, no audio gaps, and no decode errors. Thus the usable-cap result does not depend on compressing the test tone into comfort-noise traffic. See the [effort comparison](../docs/benchmarks/optimization/effort-comparison.json).


A matched Original/Tiny run measured 13.2% median process-tree CPU and 154 MiB peak summed RSS in debug, versus 4.8% and about 125 MiB in release. Bandwidth and received video frames were effectively unchanged. This supports using the release build for manual performance testing; these short sequential runs are approximate measurements, not a universal percentage guarantee. [Debug comparison](../docs/benchmarks/optimization/debug/comparison.json).

## Validation and artifacts

- All 14 Rust tests passed, including actual Opus encode/decode at every packet size, sustained silence, resumed tone, packet loss concealment, and finite decoded samples.
- Clippy with warnings denied, formatting, debug/release builds, Python syntax checks, and four existing Python result-classification tests passed.
- Seventeen comparison cases ran across the main and effort suites. All transport checks passed. Baselines and 64 kbit/s candidates additionally met the audio-sample/video-frame/no-audio-gap delivery thresholds. The 48 kbit/s case is explicitly marked inadequate rather than promoted as a success.
- Mixed 20/60 ms peers passed the real call/relay check.
- The P0 suite using Minimum passed audio exchange, mute/unmute, stats, leave, timeout cleanup, same-ID reuse, forced-peer-loss cleanup, lease expiry, and signaling failure handling.
- The built-in-speaker checks using Minimum, effort 5, passed for both audio-only and simultaneous video after the playback reserve fix: zero missing, dropped, late, or device-error samples in each 15-second check. These are continuity checks, not a listening-quality score.
- Two native release GUI clients using Minimum, effort 5, DTX disabled, Tiny, and two-second refresh completed a 20-second check. Each received 337 audio packets and 101 video frames with no audio gaps or decode errors. The new controls were visually inspected in the [GUI screenshot](../docs/images/native-call.png). Advanced tuning is collapsed to keep ordinary controls accessible.
- The local `target/RCOF.app` bundle was rebuilt from the release executable. All test processes were stopped.

## What to try next by ear

```sh
python3 scripts/demo_p1.py --binary target/release/rcof --audio-profile minimum --quality tiny --keyframe-seconds 2
```

Start with encoding effort 10, which remains the default. Leave both calls, select microphone/camera and an output device, then join both again. Compare Original, Efficient, and Minimum using the same microphone and speaking position. For a separate CPU experiment, try effort 5 in Audio tuning; use Keep quiet audio to compare without DTX.

Minimum adds up to 40 ms of packet assembly time relative to Original, and longer-packet buffering may increase startup/rebuffer delay. A lost Minimum packet loses 60 ms of sound instead of 20 ms. Two-second video refreshes can delay picture recovery. The original audio target is unchanged in Efficient; Minimum and lower encoding effort still need the user's listening check. These experiments used generated signals, not a real-speech quality study or a real-camera movement benchmark. Earlier intermittent behavior on another speaker output is not claimed fixed by these built-in-speaker checks.

At the time of this optimization step, GitHub publication had not yet happened. The project remains a two-instance local hobby prototype.
