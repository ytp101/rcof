//! VP8 media helpers. FFmpeg owns codecs/capture; Rust owns buffering, RTP and lifetime.
use crate::metrics::{Metrics, add, monotonic_us};
use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use clap::ValueEnum;
use std::{
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{mpsc, watch},
    task::JoinSet,
};
use webrtc::{
    rtp::{
        codecs::vp8::{Vp8Packet, Vp8Payloader},
        header::Header,
        packet::Packet,
        packetizer::{Depacketizer, Payloader},
    },
    rtp_transceiver::rtp_receiver::RTCRtpReceiver,
    track::{
        track_local::{TrackLocalWriter, track_local_static_rtp::TrackLocalStaticRTP},
        track_remote::TrackRemote,
    },
};

pub const PLAYOUT_US: u64 = 120_000;
pub const TIME_URI: &str = "urn:rcof:same-host-video-created-us";
const MAX_ENCODED: usize = 2 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Default, PartialEq, ValueEnum)]
pub enum VideoSource {
    #[default]
    Off,
    Pattern,
    Camera,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, ValueEnum)]
#[repr(u64)]
pub enum Quality {
    Tiny,
    Minimal,
    Low,
    #[default]
    Standard,
}
impl Quality {
    pub fn from_level(level: u64) -> Self {
        match level {
            0 => Self::Tiny,
            1 => Self::Minimal,
            2 => Self::Low,
            _ => Self::Standard,
        }
    }
    pub fn dimensions(self) -> (u32, u32) {
        match self {
            Self::Tiny => (160, 90),
            Self::Minimal => (256, 144),
            Self::Low => (320, 180),
            Self::Standard => (640, 360),
        }
    }
    pub fn bitrate(self) -> u32 {
        match self {
            Self::Tiny => 40_000,
            Self::Minimal => 80_000,
            Self::Low => 180_000,
            Self::Standard => 450_000,
        }
    }
    pub fn fps(self) -> u32 {
        match self {
            Self::Tiny => 5,
            Self::Minimal => 10,
            _ => 15,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Tiny => "Tiny · 160 × 90 · 5 fps",
            Self::Minimal => "Minimal · 256 × 144 · 10 fps",
            Self::Low => "Low · 320 × 180",
            Self::Standard => "Standard · 640 × 360",
        }
    }
}
#[derive(Clone, Debug, clap::Args)]
#[group(id = "video_options")]
pub struct Options {
    #[arg(long, value_enum, default_value = "off")]
    pub video: VideoSource,
    #[arg(long, value_enum, default_value = "standard")]
    pub quality: Quality,
    /// AVFoundation camera index (see `ffmpeg -f avfoundation -list_devices true -i ""`).
    #[arg(long, default_value = "0")]
    pub camera: String,
    #[arg(long, default_value = "ffmpeg")]
    pub ffmpeg: String,
    /// Longer intervals reduce full-picture refresh traffic but delay loss recovery.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=5))]
    pub keyframe_seconds: u32,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            video: VideoSource::Off,
            quality: Quality::Standard,
            camera: "0".into(),
            ffmpeg: "ffmpeg".into(),
            keyframe_seconds: 1,
        }
    }
}
#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub created_us: u64,
    pub received_at: Instant,
}
#[derive(Default)]
pub struct View {
    pub local: Mutex<Option<Arc<Frame>>>,
    pub remote: Mutex<Option<Arc<Frame>>>,
    pub enabled: AtomicBool,
    pub presented_id: AtomicU64,
    pub quality: AtomicU64,
}
impl View {
    pub fn clear(&self) {
        *self.local.lock().unwrap() = None;
        *self.remote.lock().unwrap() = None;
    }
}
fn command(path: &str) -> Command {
    // Finder launches may not inherit the terminal’s Homebrew PATH.
    let resolved = if path == "ffmpeg" {
        ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"]
            .into_iter()
            .find(|candidate| std::path::Path::new(candidate).is_file())
            .unwrap_or(path)
    } else {
        path
    };
    let mut c = Command::new(resolved);
    c.args(["-hide_banner", "-loglevel", "error", "-nostdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    c
}
/// Every child and async helper is retained here, even when a parent task is cancelled.
struct Processes {
    children: Vec<Child>,
    tasks: JoinSet<Result<()>>,
}
impl Processes {
    fn new() -> Self {
        Self {
            children: vec![],
            tasks: JoinSet::new(),
        }
    }
    fn keep(&mut self, mut child: Child) -> usize {
        if let Some(mut stderr) = child.stderr.take() {
            self.tasks.spawn(async move {
                let mut chunk = [0u8; 1024];
                loop {
                    let n = stderr.read(&mut chunk).await?;
                    if n == 0 {
                        return Ok(());
                    }
                    eprintln!("video helper: {}", String::from_utf8_lossy(&chunk[..n]));
                }
            });
        }
        self.children.push(child);
        self.children.len() - 1
    }
    async fn stop(&mut self) {
        self.tasks.abort_all();
        for child in &mut self.children {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        while self.tasks.join_next().await.is_some() {}
        self.children.clear();
    }
}
// Child::kill_on_drop and JoinSet::drop also cover cancellation/error paths.

pub fn test_pattern(width: u32, height: u32, index: u64, tint: u8) -> Vec<u8> {
    let mut rgb = vec![0; (width * height * 3) as usize];
    let bars = [
        [54, 117, 255],
        [25, 194, 162],
        [252, 190, 69],
        [241, 98, 108],
        [137, 99, 231],
        [231, 237, 248],
    ];
    for y in 0..height {
        for x in 0..width {
            let i = ((y * width + x) * 3) as usize;
            let bar = ((x * 6 / width) as usize + tint as usize) % 6;
            let grid = (x / 32 + y / 32) % 2;
            for c in 0..3 {
                rgb[i + c] = ((bars[bar][c] as u32 * (70 + grid * 10)) / 100) as u8;
            }
            let position = (index * 7 % width as u64) as i32;
            if (x as i32 - position).abs() < 6 {
                rgb[i..i + 3].copy_from_slice(&[255, 255, 255]);
            }
            // Alternating central block gives a visible timing/motion reference.
            if x > width / 3 && x < 2 * width / 3 && y > height / 3 && y < 2 * height / 3 {
                rgb[i..i + 3].copy_from_slice(if (index / 15).is_multiple_of(2) {
                    &[24, 31, 44]
                } else {
                    &[241, 246, 255]
                });
            }
        }
    }
    rgb
}

fn ivf_header(width: u32, height: u32) -> [u8; 32] {
    let mut h = [0; 32];
    h[..4].copy_from_slice(b"DKIF");
    h[6..8].copy_from_slice(&32u16.to_le_bytes());
    h[8..12].copy_from_slice(b"VP80");
    h[12..14].copy_from_slice(&(width as u16).to_le_bytes());
    h[14..16].copy_from_slice(&(height as u16).to_le_bytes());
    h[16..20].copy_from_slice(&15u32.to_le_bytes());
    h[20..24].copy_from_slice(&1u32.to_le_bytes());
    h
}
fn keyframe_size(frame: &[u8]) -> Option<(u32, u32)> {
    if frame.len() < 10 || frame[0] & 1 != 0 || frame[3..6] != [0x9d, 0x01, 0x2a] {
        return None;
    }
    let w = u16::from_le_bytes([frame[6], frame[7]]) & 0x3fff;
    let h = u16::from_le_bytes([frame[8], frame[9]]) & 0x3fff;
    ((1..=1280).contains(&w) && (1..=720).contains(&h)).then_some((w as u32, h as u32))
}

pub async fn send(
    options: Options,
    id: String,
    track: Arc<TrackLocalStaticRTP>,
    extension: Option<u8>,
    view: Arc<View>,
    metrics: Arc<Metrics>,
) -> Result<()> {
    let mut sequence = 0u16;
    let mut rtp_time = 0u32;
    loop {
        if !view.enabled.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(30)).await;
            continue;
        }
        let mut current = options.clone();
        current.quality = Quality::from_level(view.quality.load(Ordering::Relaxed));
        let mut processes = Processes::new();
        let result = tokio::select! {
            r=send_enabled(&current,&id,&track,extension,&view,&metrics,&mut processes,&mut sequence,&mut rtp_time)=>r,
            _=wait_changed(&view,current.quality)=>Ok(()),
        };
        processes.stop().await;
        *view.local.lock().unwrap() = None;
        result?;
    }
}
async fn wait_changed(view: &View, quality: Quality) {
    while view.enabled.load(Ordering::Relaxed)
        && view.quality.load(Ordering::Relaxed) == quality as u64
    {
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
#[allow(clippy::too_many_arguments)]
async fn send_enabled(
    options: &Options,
    id: &str,
    track: &TrackLocalStaticRTP,
    extension: Option<u8>,
    view: &Arc<View>,
    metrics: &Arc<Metrics>,
    processes: &mut Processes,
    sequence: &mut u16,
    rtp_time: &mut u32,
) -> Result<()> {
    let (width, height) = options.quality.dimensions();
    let size = format!("{width}x{height}");
    let bitrate = options.quality.bitrate().to_string();
    let fps = options.quality.fps();
    let fps_text = fps.to_string();
    let keyframe_frames = (fps * options.keyframe_seconds).to_string();
    let mut enc = command(&options.ffmpeg);
    enc.args([
        "-probesize",
        "32",
        "-analyzeduration",
        "0",
        "-f",
        "rawvideo",
        "-pixel_format",
        "rgb24",
        "-video_size",
        &size,
        "-framerate",
        &fps_text,
        "-i",
        "pipe:0",
        "-an",
        "-c:v",
        "libvpx",
        "-deadline",
        "realtime",
        "-cpu-used",
        "8",
        "-threads",
        "2",
        "-lag-in-frames",
        "0",
        "-auto-alt-ref",
        "0",
        "-g",
        &keyframe_frames,
        "-b:v",
        &bitrate,
        "-pix_fmt",
        "yuv420p",
        "-flush_packets",
        "1",
        "-f",
        "ivf",
        "pipe:1",
    ]);
    let index = processes.keep(
        enc.spawn()
            .context("cannot start FFmpeg VP8 encoder; install ffmpeg or set --ffmpeg")?,
    );
    let mut input = processes.children[index].stdin.take().unwrap();
    let mut encoded = processes.children[index].stdout.take().unwrap();
    let (capture_tx, mut capture_rx) = watch::channel::<Option<Arc<Frame>>>(None);
    let tint = id.bytes().fold(0u8, |a, b| a.wrapping_add(b)) % 6;
    if options.video == VideoSource::Camera {
        let mut capture = command(&options.ffmpeg);
        // Let AVFoundation choose a supported capture size; scale after capture.
        capture.args(["-f","avfoundation","-framerate","30","-i",&format!("{}:none",options.camera),"-an","-vf",&format!("fps={fps},scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"),"-pix_fmt","rgb24","-threads","1","-f","rawvideo","pipe:1"]);
        let ci = processes.keep(capture.spawn().context("cannot start camera helper")?);
        let mut camera = processes.children[ci].stdout.take().unwrap();
        let view = view.clone();
        processes.tasks.spawn(async move {
            loop {
                let mut rgb = vec![0; (width * height * 3) as usize];
                camera
                    .read_exact(&mut rgb)
                    .await
                    .context("camera stopped; check macOS Camera permission and camera index")?;
                let frame = Arc::new(Frame {
                    width,
                    height,
                    rgb,
                    created_us: monotonic_us(),
                    received_at: Instant::now(),
                });
                *view.local.lock().unwrap() = Some(frame.clone());
                capture_tx.send_replace(Some(frame));
            }
        });
    } else {
        let view = view.clone();
        processes.tasks.spawn(async move {
            let mut timer = tokio::time::interval(Duration::from_micros(1_000_000 / fps as u64));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut number = 0;
            loop {
                timer.tick().await;
                let frame = Arc::new(Frame {
                    width,
                    height,
                    rgb: test_pattern(width, height, number, tint),
                    created_us: monotonic_us(),
                    received_at: Instant::now(),
                });
                number += 1;
                *view.local.lock().unwrap() = Some(frame.clone());
                capture_tx.send_replace(Some(frame));
            }
        });
    }
    let (stamp_tx, mut stamps) = mpsc::channel::<u64>(3);
    let metrics_writer = metrics.clone();
    processes.tasks.spawn(async move {
        loop {
            capture_rx.changed().await.context("video source ended")?;
            let frame = capture_rx
                .borrow_and_update()
                .clone()
                .context("empty capture frame")?;
            // Backpressure is bounded: at most three timestamps plus pipe buffers.
            stamp_tx
                .send(frame.created_us)
                .await
                .context("encoder reader stopped")?;
            let start = Instant::now();
            input
                .write_all(&frame.rgb)
                .await
                .context("VP8 encoder input closed")?;
            if start.elapsed() > Duration::from_millis(100) {
                add(&metrics_writer.video_backpressure, 1);
            }
        }
    });
    let mut header = [0; 32];
    tokio::time::timeout(Duration::from_secs(8), encoded.read_exact(&mut header))
        .await
        .context("video source timed out; check camera permission/device or FFmpeg")??;
    ensure!(
        &header[..4] == b"DKIF" && &header[8..12] == b"VP80",
        "unexpected encoder output"
    );
    let mut payloader = Vp8Payloader::default();
    payloader.enable_picture_id = true;
    loop {
        let mut frame_header = [0; 12];
        encoded
            .read_exact(&mut frame_header)
            .await
            .context("VP8 encoder stopped")?;
        let len = u32::from_le_bytes(frame_header[..4].try_into().unwrap()) as usize;
        ensure!(len > 0 && len <= MAX_ENCODED, "invalid VP8 frame length");
        let mut data = vec![0; len];
        encoded.read_exact(&mut data).await?;
        let created = stamps.recv().await.context("missing video timestamp")?;
        metrics
            .video_encode
            .record(monotonic_us().saturating_sub(created));
        let fragments = payloader.payload(1000, &Bytes::from(data))?;
        let count = fragments.len();
        for (i, payload) in fragments.into_iter().enumerate() {
            let mut packet = Packet {
                header: Header {
                    version: 2,
                    sequence_number: *sequence,
                    timestamp: *rtp_time,
                    marker: i + 1 == count,
                    ..Default::default()
                },
                payload,
            };
            if let Some(e) = extension {
                packet
                    .header
                    .set_extension(e, Bytes::copy_from_slice(&created.to_be_bytes()))?;
            }
            tokio::time::timeout(Duration::from_millis(200), track.write_rtp(&packet))
                .await
                .context("video transport stalled")??;
            *sequence = sequence.wrapping_add(1);
        }
        *rtp_time = rtp_time.wrapping_add(90_000 / fps);
        add(&metrics.video_tx_frames, 1);
        add(&metrics.video_tx_bytes, len);
        if !view.enabled.load(Ordering::Relaxed) {
            return Ok(());
        }
    }
}

struct EncodedFrame {
    data: Vec<u8>,
    created: u64,
}
/// A bounded frame reassembler rejects incomplete/oversized frames; periodic keyframes
/// allow recovery. This is not a P2 network adaptation implementation.
#[derive(Default)]
struct Reassembler {
    timestamp: Option<u32>,
    next: u16,
    bytes: Vec<u8>,
    created: u64,
    waiting_key: bool,
    last_sequence: Option<u16>,
}
impl Reassembler {
    fn push(&mut self, p: &Packet, created: u64) -> Result<Option<EncodedFrame>> {
        if self
            .last_sequence
            .is_some_and(|last| p.header.sequence_number != last.wrapping_add(1))
        {
            self.waiting_key = true;
        }
        self.last_sequence = Some(p.header.sequence_number);
        let mut descriptor = Vp8Packet::default();
        let payload = descriptor.depacketize(&p.payload)?;
        if descriptor.s == 1 && descriptor.pid == 0 {
            self.timestamp = Some(p.header.timestamp);
            self.next = p.header.sequence_number;
            self.bytes.clear();
            self.created = created;
        }
        if self.timestamp != Some(p.header.timestamp) || p.header.sequence_number != self.next {
            self.timestamp = None;
            self.bytes.clear();
            self.waiting_key = true;
            return Ok(None);
        }
        self.next = self.next.wrapping_add(1);
        if self.bytes.len() + payload.len() > MAX_ENCODED {
            bail!("remote video frame too large");
        }
        self.bytes.extend_from_slice(&payload);
        if p.header.marker {
            self.timestamp = None;
            let data = std::mem::take(&mut self.bytes);
            if self.waiting_key && keyframe_size(&data).is_none() {
                return Ok(None);
            }
            self.waiting_key = false;
            return Ok(Some(EncodedFrame {
                data,
                created: self.created,
            }));
        }
        Ok(None)
    }
}

pub async fn receive(
    track: Arc<TrackRemote>,
    receiver: Arc<RTCRtpReceiver>,
    ffmpeg: String,
    view: Arc<View>,
    metrics: Arc<Metrics>,
) -> Result<()> {
    let extension = receiver
        .get_parameters()
        .await
        .header_extensions
        .iter()
        .find(|e| e.uri == TIME_URI)
        .map(|e| e.id as u8);
    let mut processes = Processes::new();
    let result = receive_inner(track, extension, &ffmpeg, view, metrics, &mut processes).await;
    processes.stop().await;
    result
}
async fn receive_inner(
    track: Arc<TrackRemote>,
    extension: Option<u8>,
    ffmpeg: &str,
    view: Arc<View>,
    metrics: Arc<Metrics>,
    processes: &mut Processes,
) -> Result<()> {
    let mut reassembler = Reassembler {
        waiting_key: true,
        ..Default::default()
    };
    let mut writer = None;
    let mut stamp_sender = None;
    let mut number = 0u64;
    let mut dimensions = None;
    loop {
        let (packet, _) = track.read_rtp().await.context("remote video track ended")?;
        let created = extension
            .and_then(|e| packet.header.get_extension(e))
            .and_then(|b| <[u8; 8]>::try_from(b.as_ref()).ok())
            .map(u64::from_be_bytes)
            .unwrap_or(0);
        let Some(frame) = reassembler.push(&packet, created)? else {
            continue;
        };
        if let Some(size) = keyframe_size(&frame.data) {
            if dimensions.is_some_and(|previous| previous != size) {
                processes.stop().await;
                writer = None;
                stamp_sender = None;
                number = 0;
            }
            dimensions = Some(size);
        }
        if writer.is_none() {
            let Some((width, height)) = keyframe_size(&frame.data) else {
                continue;
            };
            let mut decoder = command(ffmpeg);
            decoder.args([
                "-probesize",
                "32",
                "-analyzeduration",
                "0",
                "-threads",
                "1",
                "-f",
                "ivf",
                "-i",
                "pipe:0",
                "-an",
                "-threads",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-fps_mode",
                "passthrough",
                "-flush_packets",
                "1",
                "pipe:1",
            ]);
            let di = processes.keep(decoder.spawn().context("cannot start FFmpeg VP8 decoder")?);
            let mut input = processes.children[di].stdin.take().unwrap();
            let mut raw = processes.children[di].stdout.take().unwrap();
            input.write_all(&ivf_header(width, height)).await?;
            writer = Some(input);
            let (tx, mut timestamps) = mpsc::channel::<u64>(8);
            stamp_sender = Some(tx);
            let view = view.clone();
            let metrics = metrics.clone();
            processes.tasks.spawn(async move {
                loop {
                    let mut rgb = vec![0; (width * height * 3) as usize];
                    raw.read_exact(&mut rgb)
                        .await
                        .context("video decoder stopped")?;
                    let created = timestamps
                        .recv()
                        .await
                        .context("decoder timestamp channel ended")?;
                    let now = monotonic_us();
                    if created > 0 && now >= created {
                        metrics.video_frame_to_decode.record(now - created);
                        metrics.video_age_us.store(now - created, Ordering::Relaxed);
                        let audio_age = metrics.audio_age_us.load(Ordering::Relaxed);
                        if audio_age > 0 {
                            metrics
                                .av_decode_skew
                                .record((now - created).abs_diff(audio_age));
                        }
                    }
                    // Same-host presentation deadline shared with scheduled audio samples.
                    if created > 0 {
                        let remaining = (created + PLAYOUT_US).saturating_sub(monotonic_us());
                        tokio::time::sleep(Duration::from_micros(remaining)).await;
                        metrics
                            .video_ready_age
                            .record(monotonic_us().saturating_sub(created));
                    }
                    add(&metrics.video_rx_frames, 1);
                    *view.remote.lock().unwrap() = Some(Arc::new(Frame {
                        width,
                        height,
                        rgb,
                        created_us: created,
                        received_at: Instant::now(),
                    }));
                }
            });
        }
        stamp_sender.as_ref().unwrap().send(frame.created).await?;
        let input = writer.as_mut().unwrap();
        input
            .write_all(&(frame.data.len() as u32).to_le_bytes())
            .await?;
        input.write_all(&number.to_le_bytes()).await?;
        input.write_all(&frame.data).await?;
        number += 1;
        add(&metrics.video_rx_bytes, frame.data.len());
        if let Some(result) = processes.tasks.try_join_next() {
            result.context("video decoder task panicked")??;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vp8_tiny_tail_packets_decode() {
        for size in 990..1010 {
            let raw = Bytes::from(vec![0x55; size]);
            let mut payloader = Vp8Payloader::default();
            payloader.enable_picture_id = true;
            let fragments = payloader.payload(1000, &raw).unwrap();
            let mut receiver = Reassembler::default();
            let mut output = None;
            for (i, payload) in fragments.iter().enumerate() {
                output = receiver
                    .push(
                        &Packet {
                            header: Header {
                                timestamp: 1,
                                sequence_number: i as u16,
                                marker: i + 1 == fragments.len(),
                                ..Default::default()
                            },
                            payload: payload.clone(),
                        },
                        10,
                    )
                    .unwrap();
            }
            assert_eq!(output.unwrap().data, raw);
        }
    }
    #[test]
    fn vp8_fragments_reassemble_and_missing_packet_is_rejected() {
        let raw = Bytes::from(vec![0x55; 5000]);
        let fragments = Vp8Payloader::default().payload(1000, &raw).unwrap();
        let mut complete = Reassembler::default();
        let mut broken = Reassembler::default();
        let mut output = None;
        for (i, payload) in fragments.iter().enumerate() {
            let p = Packet {
                header: Header {
                    timestamp: 1,
                    sequence_number: i as u16,
                    marker: i + 1 == fragments.len(),
                    ..Default::default()
                },
                payload: payload.clone(),
            };
            output = complete.push(&p, 10).unwrap();
            if i != 1 {
                assert!(broken.push(&p, 10).unwrap().is_none());
            }
        }
        assert_eq!(output.unwrap().data, raw);
    }
}

/// Conservative loss-based ladder: fast decrease, slow recovery to the user's ceiling.
#[derive(Default)]
struct Adaptation {
    bad: u8,
    good: u8,
}
impl Adaptation {
    fn observe(&mut self, loss: u8, current: u64, ceiling: u64) -> u64 {
        if loss >= 13 {
            // >=5% loss in an RTCP reporting interval
            self.bad = self.bad.saturating_add(1);
            self.good = 0;
            if self.bad >= 2 {
                self.bad = 0;
                return current.saturating_sub(1);
            }
        } else if loss <= 2 {
            self.bad = 0;
            self.good = self.good.saturating_add(1);
            if self.good >= 8 {
                self.good = 0;
                return (current + 1).min(ceiling);
            }
        } else {
            self.bad = 0;
            self.good = 0;
        }
        current
    }
}
pub async fn feedback(
    sender: Arc<webrtc::rtp_transceiver::rtp_sender::RTCRtpSender>,
    view: Arc<View>,
    metrics: Arc<Metrics>,
    adaptive: bool,
    ceiling: Quality,
) -> Result<()> {
    let mut policy = Adaptation::default();
    let ssrc = sender
        .get_parameters()
        .await
        .encodings
        .first()
        .map(|encoding| encoding.ssrc);
    loop {
        let (packets, _) = sender.read_rtcp().await?;
        for packet in packets {
            let reports = if let Some(rr) = packet
                .as_any()
                .downcast_ref::<webrtc::rtcp::receiver_report::ReceiverReport>(
            ) {
                &rr.reports
            } else if let Some(sr) = packet
                .as_any()
                .downcast_ref::<webrtc::rtcp::sender_report::SenderReport>()
            {
                &sr.reports
            } else {
                continue;
            };
            for report in reports {
                if Some(report.ssrc) != ssrc {
                    continue;
                }
                metrics
                    .video_loss_fraction
                    .store(report.fraction_lost as u64, Ordering::Relaxed);
                if adaptive {
                    let old = view.quality.load(Ordering::Relaxed);
                    let next = policy.observe(report.fraction_lost, old, ceiling as u64);
                    if next != old {
                        view.quality.store(next, Ordering::Relaxed);
                        metrics.video_quality_level.store(next, Ordering::Relaxed);
                        add(&metrics.video_quality_changes, 1);
                        println!(
                            "video adaptation: {} → {} (loss {}/256)",
                            Quality::from_level(old).label(),
                            Quality::from_level(next).label(),
                            report.fraction_lost
                        );
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod adaptation_tests {
    use super::*;
    #[test]
    fn adaptation_has_hysteresis_and_respects_ceiling() {
        let mut a = Adaptation::default();
        assert_eq!(a.observe(20, 3, 3), 3);
        assert_eq!(a.observe(20, 3, 3), 2);
        for _ in 0..7 {
            assert_eq!(a.observe(0, 2, 3), 2);
        }
        assert_eq!(a.observe(0, 2, 3), 3);
        for _ in 0..16 {
            assert_eq!(a.observe(0, 1, 1), 1);
        }
    }
}
