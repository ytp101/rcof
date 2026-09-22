# Run P1: native audio/video

P1 runs two native Rust windows on one Mac. It requires Rust, the P0 Opus development library, and FFmpeg with libvpx and AVFoundation support. This machine was tested with Rust 1.91, Opus 1.6.1 and FFmpeg 8.1.1. On a Homebrew Mac, install the native dependencies with `brew install opus ffmpeg`.

```sh
cargo build --locked
python3 scripts/demo_p1.py
```

The launcher starts a loopback signaling service and two windows in the same room. Both initially send generated audio and moving video patterns; playback is silent. Close both windows or press Ctrl-C in the launcher terminal to stop everything. Logs and final measurements go to `runs/p1-demo/`.

First builds download dependencies from crates.io. After fetching dependencies, `cargo build --locked --offline` needs no network. Runtime media and signaling use loopback only.

## Devices and controls

Leave the call before changing devices. Choose Microphone and Headphones / speaker for real audio. Use headphones with a microphone: acoustic echo cancellation is not implemented. Choose Camera on one instance and Test pattern on the other to avoid competing for the same camera. macOS may request device permission for the launching terminal or app.

“No outgoing video” disables local capture/sending while allowing the other window’s video to be received. Both windows still negotiate VP8 capability.

Join uses the instance name and room shown in the sidebar. Names must differ. During a call, use Mute audio, Turn video off/on, and Leave call. A departing peer ends the call for the other participant; both can join again. Quality and device changes take effect on the next call.

| Preset | Dimensions | Frames/second | Encoder target |
| --- | --- | --- | --- |
| Tiny | 160 × 90 | 5 | 40 kbit/s |
| Minimal | 256 × 144 | 10 | 80 kbit/s |
| Low | 320 × 180 | 15 | 180 kbit/s |
| Standard | 640 × 360 | 15 | 450 kbit/s |

Targets are encoder settings, not bandwidth caps. Actual rates depend on content and include additional audio/transport overhead.

## Manual launch

Run these in separate terminals:

```sh
./target/debug/rcof server
./target/debug/rcof gui --id a
./target/debug/rcof gui --id b
```

A CLI video call is also available: `./target/debug/rcof call --id a --video pattern --quality low`. Use `--video camera --camera 0` for a camera. CLI defaults remain audio-only. `--ffmpeg /absolute/path/to/ffmpeg` overrides the executable. Standard Homebrew paths are detected for Finder launches.

`bash scripts/package_macos.sh` creates `target/RCOF.app` with microphone/camera usage descriptions. Add `--release` for an optimized bundle. This is a local bundle; it is not signed for distribution and does not include FFmpeg or Opus. It opens one window and requires a separately running signaling service. The Python launcher is the simplest two-window demonstration.

## Verification

```sh
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
python3 scripts/check_p1.py --duration 60
python3 scripts/check_p1.py --quality low --duration 30
python3 scripts/check_p1.py --camera --duration 12 --no-controls
python3 scripts/check_p0.py --offline --duration 10
```

The generated-source P1 check applies the macOS loopback-only sandbox to the server, clients, and their FFmpeg children. It checks bidirectional audio/video and verifies that turning video off preserves audio, resuming video works, and leaving stops both clients. The optional camera client runs outside that sandbox for device access; do not describe that camera run as an offline-sandbox test.

## Timing limits

Audio and video use a shared same-Mac monotonic clock and a 120 ms presentation target when video is enabled. Audio applies that target at startup or rebuffering, then uses the device sample clock for uninterrupted playback. Late frames cannot meet that target. Video is published to the UI after the target time; actual screen refresh comes later. Camera timestamps begin after raw capture reaches Rust, so they exclude camera exposure and part of capture buffering. Audio metrics also do not measure acoustic mouth-to-ear latency.

JSON histograms report upper bounds of power-of-two buckets. `video_frame_to_decode` measures creation-to-decoding, `video_ready_age` includes the presentation wait, and `av_decode_skew` compares recent decoding ages. None proves physical lip-sync. [P2](running-p2.md) now provides local impairment and loss-based adaptation experiments; these do not establish real-network reliability.

For the learning walkthrough, read [the P1 code tour](p1-code-tour.md). `python3 scripts/check_p1_speaker.py` additionally plays a short test tone through the default output while receiving video.

Audio continuity regression (audible tone):

```sh
python3 scripts/check_p1_speaker.py --duration 60
python3 scripts/check_p1_speaker.py --poll-delay-ms 120 --output runs/slow-signaling
```

The second command delays signaling responses through a local HTTP proxy. It does not impair media packets and is not the P2 network simulator. Both checks require zero missing, dropped, or over-20-ms-late playback samples on the selected default audio output.

Run `python3 scripts/check_video_negotiation.py` to check one-way video in both offer directions and a call with neither side sending video. This generated-source test blocks external networking.
