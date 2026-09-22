# P0 Rust code tour

Read the code in this order:

1. [`src/main.rs`](../src/main.rs): a CLI enum chooses `server`, `devices`, or `call`. `Result` lets errors travel to the terminal with context.
2. [`src/signaling.rs`](../src/signaling.rs): structs represent join requests and responses. One shared, mutex-protected room map handles short synchronous state changes; the lock is not held across network awaits. Sessions have bounded message queues and expiry.
3. [`src/audio.rs`](../src/audio.rs): `Audio` owns the device streams. Dropping that value releases them. `Arc` shares bounded sample queues between audio callbacks and the call task without copying ownership of the device. Generic callback functions convert different hardware sample types to/from `f32`.
4. [`src/call.rs`](../src/call.rs): `tokio::select!` coordinates connection events, terminal commands, timers, and incoming tracks. The sender reuses a PCM buffer and an Opus output buffer. A `JoinSet` tracks background readers; shutdown aborts and joins those tasks and closes the connection.
5. [`src/metrics.rs`](../src/metrics.rs): atomic counters can be updated from device callbacks without taking a mutex. Timing histograms have fixed storage. The only explicit unsafe call reads the system monotonic clock; the comment records why its pointer use is valid.

## One audio frame

In Original mode, the source supplies 960 mono samples at 48 kHz (20 ms). Optional profiles now use 40 or 60 ms. The client encodes them with Opus, attaches a same-host timing extension, and writes one RTP packet into the WebRTC track. WebRTC handles its DTLS/SRTP transport. The other client reads RTP, decodes Opus, updates counters, and either discards the decoded samples in `null` mode or queues them for device playback.

`mute` fills the source frame with zeroes before encoding. That preserves packet flow and makes mute behavior testable without tearing down the connection.

## Ownership and bounded work

Audio callbacks do no network I/O and allocate no per-frame collections. Capture downmixes to mono and performs a simple streaming linear sample-rate conversion. Queue overflow discards the oldest sample to keep memory bounded and avoid an indefinitely growing delay. Output underflow produces silence and waits for the prebuffer again.

This is a baseline for the same-Mac experiment. Linear interpolation is not a production-quality antialiasing resampler, sample-level overflow can click, and device-clock drift can eventually cause underflow or overflow. The counters expose those limitations for future improvements.

## Why existing media libraries

P0 uses a pinned classic WebRTC.rs API (`webrtc` 0.14.0), CPAL 0.16.0, and `opus` 0.3.1. They are the tested compatibility baseline, not a claim to use the newest release. The application learns Rust by integrating these pieces; it does not implement codecs or cryptography.

## Boundaries of the result

P0 was the audio-only milestone. The current app also has native video, a GUI, adaptation, and limited loss/reordering recovery. Echo cancellation, automatic reconnection, and public-network operation remain outside the implemented scope. Connection setup and ICE candidates are restricted to loopback; the signaling service is not an Internet-facing service.
