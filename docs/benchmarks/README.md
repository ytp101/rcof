# Benchmark evidence

These are curated measurements from two native processes on one Apple Silicon Mac, using generated tone/silence and video patterns. They are not recordings of a person or a production network.

- [Main comparison](optimization/comparison.json): original/efficient/minimum modes, link caps, silence, loss, and outage cases.
- [Encoding-effort comparison](optimization/effort-comparison.json): disables DTX to avoid treating constant-tone suppression as a speech-bandwidth improvement.
- [Debug comparison](optimization/debug/comparison.json): the same original Tiny configuration using the debug build.
- [Run metadata](optimization/metadata.json) and [follow-up metadata](optimization/effort-metadata.json): toolchain/build fingerprints and the change between suites.

Each case directory retains client summaries and relay configuration/counters where available. Absolute project/interpreter paths in commands have been made relative/portable. Raw process listings and logs stay local; the comparison files retain aggregate resource measurements. Build fingerprints identify the measured binaries, not a promise that independently rebuilt binaries will have identical hashes.

Rates are per direction. Relay totals include IPv4/UDP, encrypted media, connection setup, and control traffic; steady measurements exclude startup/teardown intervals. Wi-Fi/VPN overhead and HTTP signaling are not included. Resource figures are approximate full-process-tree OS estimates. A lower delivered rate under congestion may mean lost media—always inspect received samples and video frames.

Reproduce with `cargo build --release --locked`, then `python3 scripts/compare_bandwidth.py`. See the [optimization report](../../reports/018-bandwidth-optimization.md) for interpretation and limitations.
