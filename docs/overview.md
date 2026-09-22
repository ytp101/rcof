# Project overview

RCOF is a personal hobby project for learning Rust through real-time audio and video. The central question is: how little traffic can a useful call consume, and what do latency, quality, and CPU cost in return?

The working setup is deliberately small: two native client processes and a signaling service on the same Mac. Clients use loopback WebRTC for encrypted media. An optional Rust UDP relay simulates bandwidth limits, delay, jitter, loss, and outages. Initial dependency installation needs Internet access; a built local call does not.

P0 established audio and cleanup. P1 added a native UI and video. P2 added the network simulator, video adaptation, and short-outage recovery. The optional optimization modes explore longer audio packets, lower codec bitrate, silence compression, and less frequent video refreshes. The original settings remain available for comparison.

Learning is the primary purpose. Existing libraries handle codecs, transport, and platform APIs so the project can focus on ownership, concurrency, buffering, lifecycle, debugging, and honest measurement. It is not a cross-device or production conferencing product.

Start with the [learning report](../reports/019-learning-report.md), then the [architecture](architecture-proposal.md). The [README](../README.md) contains the shortest runnable example.
