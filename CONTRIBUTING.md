# Working on RCOF

Keep changes small enough to explain and measure. This repository targets same-Mac learning experiments; extending it to public networks is an architectural change, not just removing an address check.

## Development

Install the Rust toolchain specified in `rust-toolchain.toml`, Xcode command-line tools, Python 3.10+, and `brew install opus ffmpeg`. Build once online:

```sh
cargo build --locked
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 -m unittest discover -s scripts -p 'test_*.py'
```

After dependencies are cached, Rust commands can add `--offline`. The release build is recommended for performance measurements; debug and release CPU costs differ substantially.

## End-to-end checks

```sh
python3 scripts/check_p0.py --offline --duration 10
python3 scripts/check_p1.py --quality low --duration 20
python3 scripts/check_video_negotiation.py
python3 scripts/check_p2.py --quality tiny --audio-profile minimum --duration 20
python3 scripts/check_p2.py --profile outage --quality tiny --duration 24
```

These generated-source checks use no microphone. macOS sandboxing blocks non-loopback networking for the lab checks. Speaker and camera checks are manual/opt-in; they require device access and are not run in CI.

For benchmarks, build release first and run `python3 scripts/compare_bandwidth.py`. Cases run sequentially. Save the exact command and configuration, distinguish transmitted from delivered traffic, and inspect frame/sample counts before calling a smaller traffic counter an improvement.

## What belongs in a change

Explain the behavior, the tradeoff, and the relevant checks. Preserve an existing mode when trying a quality/latency tradeoff. Prefer bounded work in callbacks and media paths. Avoid holding a mutex across an `await`, and ensure owned tasks/child processes stop when a call ends.

Keep logs, generated runs, builds, device recordings, credentials, and machine-specific configuration out of commits. Curated benchmark evidence lives in `docs/benchmarks/`; raw experiment files stay under ignored `runs/` or local reports. Add a short learning note when a change reveals something useful.

The macOS CI workflow checks formatting, unit tests, clippy, Python tests, and two synthetic two-peer calls. Passing CI does not establish microphone quality, lip-sync, long-call stability, or behavior on other operating systems.
