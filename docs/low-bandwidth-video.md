# How small can the video be?

**RCOF's smallest selectable preset is currently 160×90 at 5 frames/second.** That is the lowest implemented preset, not a universal codec minimum or a guarantee that faces will remain clear. It favors a small, slow-moving head-and-shoulders view. It is unsuitable for reading shared documents or fine detail. The user reports that the original app has clear audio and an understandable low-detail picture in their manual test; this is subjective feedback, not a controlled quality benchmark.

Lower resolution alone does not determine bandwidth. Frame rate, motion, lighting noise, keyframe frequency, codec settings, audio, and packet overhead all matter. FFmpeg's libvpx `b:v` is a target bitrate; it is not a strict network bandwidth cap. RCOF measures the resulting traffic separately. See the [FFmpeg libvpx documentation](https://ffmpeg.org/ffmpeg-codecs.html#libvpx) and [VP8 RTP format](https://www.rfc-editor.org/rfc/rfc7741.html).

## Available presets

| Preset | Resolution | Frame rate | VP8 encoder target |
| --- | --- | --- | --- |
| Tiny | 160×90 | 5 fps | 40 kbit/s |
| Minimal | 256×144 | 10 fps | 80 kbit/s |
| Low | 320×180 | 15 fps | 180 kbit/s |
| Standard | 640×360 | 15 fps | 450 kbit/s |

The selected preset is the ceiling when adaptation is enabled. The app may step down or gradually increase back toward that ceiling based on remote loss reports. Turn adaptation off before joining to keep a preset fixed.

## Measurements from the local generated-source tests

Rates below are **per sending direction**. The IP figures include encrypted media/control, a 20-byte IPv4 header, and an 8-byte UDP header for each datagram. They exclude Ethernet/Wi-Fi/VPN overhead. The application transport rate and IP rate deliberately differ.

| Mode | Video payload, session average | App transport, session average | Steady delivered IPv4/UDP traffic |
| --- | --- | --- | --- |
| Audio only, Opus target 24 kbit/s | None | About 43 kbit/s | About 54 kbit/s |
| Tiny | About 17–19 kbit/s | About 62–63 kbit/s | About 76–77 kbit/s |
| Minimal | About 40–42 kbit/s | About 87–89 kbit/s | About 101–102 kbit/s |

These were 20-second generated tone/pattern calls, not real-camera quality measurements. Steady traffic averages use relay snapshot intervals starting between seconds 4 and 16; complete session counters include startup and teardown. Both directions normally send independently, so a shared aggregate link needs budget for both directions.

## Which link budget should I try?

- **96 kbit/s per direction:** Tiny received 100 and 91 frames over roughly 20 seconds (approximately the intended 5 fps), with no audio sequence gaps. This is a tested starting point for these synthetic sources, not a guaranteed minimum. Allow more room for camera motion, bursts, and other network overhead.
- **64 kbit/s per direction:** Audio kept flowing, but Tiny received only 12 and 6 video frames in the entire run. That is roughly 0.3–0.6 fps and is not useful continuous video. The relay discarded stale queued video to protect audio. A test labeled PASS means the call and instrumentation worked; it does not mean video quality was acceptable.
- **Lowest traffic:** Choose No outgoing video on both sides. The current 24 kbit/s Opus, 20 ms packet configuration measured approximately 54 kbit/s per direction at IPv4/UDP level. That is not Opus's theoretical minimum; we have not validated intelligibility at lower audio targets or longer packet durations.

Smaller video dimensions could be implemented, but reducing them without checking whether users can understand the picture would not establish a meaningful “minimum.” Keep Tiny as the current experimental floor and compare real faces before claiming “decent video.”

## Reproduce

```sh
cargo build --locked
python3 scripts/check_p2.py --quality tiny --duration 20 --output runs/tiny-unlimited
python3 scripts/check_p2.py --quality tiny --kbps 96 --duration 20 --output runs/tiny-96
python3 scripts/check_p2.py --quality tiny --kbps 64 --duration 20 --output runs/tiny-64
python3 scripts/check_p2.py --audio-only --duration 20 --output runs/audio-only
```

These figures record the original P2 experiments. Current curated comparisons are in the [benchmark evidence](benchmarks/README.md); use the commands above to reproduce the earlier configurations.

## Optional optimization follow-up

The table above records the original 24 kbit/s, 20 ms audio configuration. Optional longer audio packets, lower audio bitrate, silence compression, and longer video refresh intervals are now available. See [the optimization guide](optimization.md) and [Step 018](../reports/018-bandwidth-optimization.md) for their measurements and tradeoffs. The original results remain here as the baseline.
