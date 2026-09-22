# Running P0 on one Mac

This guide exercises the command-line audio path of the current app. It uses no browser or external conferencing service. Two client processes exchange Opus audio using WebRTC over IPv4 loopback; a third local process handles call setup.

## Build

Requirements: a Rust toolchain compatible with Rust 1.91 or newer and Apple command-line developer tools. Opus is provided through `audiopus_sys`, which can build the bundled native library; a system Opus installation is also supported by that dependency. A bundled-library build may require CMake. The verified development machine already had Rust, developer tools, CMake, and Opus installed.

```sh
cargo build --locked
```

First-time dependency downloads require Internet access. After building, the executable runs locally without it. `Cargo.lock` pins the tested dependency set. You can build with `cargo build --locked --offline` once dependencies are cached.

## Fast automated demonstration

From the project directory:

```sh
python3 scripts/check_p0.py --offline --duration 10
```

This launches the server and two clients, checks real Opus transmission and decoding, exercises terminal controls, and cleans up its processes. It is silent: generated tones are decoded and measured, not played. On macOS, `--offline` applies the supplied sandbox profile to deny non-loopback networking. It does not disable your Mac's Wi-Fi or change system network settings.

Artifacts go to `runs/p0-check/`. For a longer test:

```sh
python3 scripts/check_p0.py --offline --duration 600 --output runs/p0-soak
```

The sandbox profile is a macOS test aid. It is not part of the runtime deployment or a cross-platform isolation mechanism.

## Run the three processes yourself

Terminal 1 — local signaling:

```sh
./target/debug/rcof server
```

Terminal 2 — first audio participant:

```sh
./target/debug/rcof call --id a --tone-hz 440
```

Terminal 3 — second audio participant:

```sh
./target/debug/rcof call --id b --tone-hz 660
```

Both join room `demo`. The defaults are `--source tone --sink null`: neither microphone nor speaker is opened. Logs show connection state, then counters every five seconds. Add `--duration 30` to stop after 30 connected seconds, or `--stats-file runs/a.json` to save a summary; create the parent directory first.

Type a command followed by Enter in a client terminal:

| Command | Behavior |
| --- | --- |
| `mute` | Encode silence instead of microphone/tone audio; maintain the connection |
| `unmute` | Restore the source |
| `stats` | Print the current JSON measurements |
| `quit` or `leave` | Leave the room and stop devices/tasks |
| Ctrl-C | Stop and clean up |

A client waits up to 30 seconds for connection by default. Set `--connect-timeout 120` if starting the other terminal takes longer. Rooms accept two distinct IDs. When a peer leaves, the other client exits; automatic reconnection is not implemented. Start both clients again to make another call.

## Hear a tone, then try the microphone

List audio devices:

```sh
./target/debug/rcof devices
```

Use exact device names from that output. For a brief standalone output check:

```sh
python3 scripts/device_check.py speaker --device 'MacBook Pro Speakers'
```

This plays a low-amplitude generated tone for five seconds. It does not open a microphone.

For a microphone check that saves counters only and does not play or record captured audio:

```sh
python3 scripts/device_check.py mic --device 'MacBook Pro Microphone'
```

macOS may ask for microphone permission for the terminal or application launching the process. Grant it if you want to use the microphone. Silence may also mean a muted device; counters are not a subjective speech-quality test.

To hear your microphone through the remote client, **put on headphones first**, keep the server running, and run:

```sh
# Terminal 2: capture and transmit; no local playback
./target/debug/rcof call --id a --source mic --sink null

# Terminal 3: decode and play the other participant into your headphones
./target/debug/rcof call --id b --source tone --sink speaker
```

Use `--input-device 'Exact name'` and `--output-device 'Exact name'` if the system defaults are not the devices you want. This is a same-Mac listening exercise, not two independent people in a call. Do not route the microphone back to nearby speakers: echo cancellation is not part of P0.

## What the measurements mean

- `tx_opus_bytes` / `rx_opus_bytes`: encoded audio payload only. They exclude RTP headers/extensions, encryption, UDP/IP, RTCP, and HTTP signaling. The reported average kbit/s divides by total session elapsed time, including setup.
- `tx_frames` / `rx_frames`: sent and successfully decoded audio packets; Original uses 50 packets/second with 20 ms frames; optional profiles use fewer, longer packets. `rx_audio_samples` measures decoded duration across profiles.
- `transport_tx_bytes` / `transport_rx_bytes`: WebRTC ICE transport counters, including encrypted media/RTCP and DTLS traffic passed through that transport. They exclude UDP/IP/link headers, signaling, and ICE connectivity-check traffic; they are not a packet-capture wire total. Transport counters refresh every five seconds and at shutdown.
- `source_rms_dbfs`: the most recent source frame’s level before mute; `null` means below meter resolution. `captured_samples` counts samples delivered by the microphone callback.
- `rx_non_silent_frames`: frames with at least one decoded sample above a small threshold. This checks signal presence, not speech intelligibility.
- `encode` / `decode`: CPU-side operation duration using a fixed histogram. Percentiles are bucket upper bounds, not exact values.
- `frame_to_decode`: same-Mac frame-assembly-to-decoder-completion time, carried in a negotiated RTP header extension using the shared system monotonic clock. Includes encoding and transport; excludes microphone hardware delay, preceding capture queue time, Opus lookahead, playback queue, and speaker delay. **It is not mouth-to-ear latency.** This extension is only meaningful on one host.
- Queue counters: missing/dropped samples and maximum observed queue size. Capture storage is capped at 100 ms; playback storage is capped at 200 ms audio-only or 240 ms with video. Playback reserves at least 40 ms and adjusts for incoming packet duration/callback size. Microphone transmission waits for one packet plus 20 ms of captured samples. Capture counters include the time spent waiting for the peer; that waiting queue is intentionally drained when connected.
- Gap/late counters: RTP sequence gaps and rejected old/duplicate packets. The current receiver waits briefly for reordered packets and conceals at most 60 ms of missing audio.

The long-test script records process CPU/RSS samples in `resources.json`. Those samples include the current machine's load and are not portable performance promises. Actual wire bandwidth and acoustic end-to-end latency have not been measured by these counters.

## Checks and troubleshooting

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

| Symptom | Check |
| --- | --- |
| Cannot reach signaling | Start `rcof server`; use the same `--server` address in both clients |
| Room full / ID already joined | Use distinct IDs; stop previous clients; crashed sessions expire after ten seconds |
| Connection timeout | Both clients need the same `--room`, different IDs, and the same server |
| Microphone open/device error | Check macOS permissions and `rcof devices`; try an explicit device name |
| Call is silent with defaults | Expected: use `--sink speaker` for playback |
| Address rejected | P0 deliberately accepts only IPv4 loopback, such as `127.0.0.1:8790` |

No automated test changes microphone permissions, disconnects your Internet, or modifies firewall settings.
