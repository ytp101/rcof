use crate::{
    audio::{self, Audio, FRAME, PlaybackSample, RATE, Resampler, Sink, Source},
    controls::CallView,
    metrics::{Metrics, add, get, monotonic_us},
    signaling, video,
};
use anyhow::{Context, Result, bail};
use bytes::Bytes;
use std::{
    io::BufRead,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
use tokio::{
    sync::mpsc,
    task::JoinSet,
    time::{MissedTickBehavior, timeout},
};
use webrtc::{
    api::{
        APIBuilder, interceptor_registry::configure_rtcp_reports, media_engine::MediaEngine,
        setting_engine::SettingEngine,
    },
    ice::{mdns::MulticastDnsMode, network_type::NetworkType},
    interceptor::registry::Registry,
    peer_connection::{
        RTCPeerConnection,
        configuration::RTCConfiguration,
        peer_connection_state::RTCPeerConnectionState,
        sdp::{sdp_type::RTCSdpType, session_description::RTCSessionDescription},
    },
    rtp::{header::Header, packet::Packet},
    rtp_transceiver::{
        RTCRtpTransceiverInit,
        rtp_codec::{
            RTCRtpCodecCapability, RTCRtpCodecParameters, RTCRtpHeaderExtensionCapability,
            RTPCodecType,
        },
        rtp_receiver::RTCRtpReceiver,
        rtp_sender::RTCRtpSender,
        rtp_transceiver_direction::RTCRtpTransceiverDirection,
    },
    track::{
        track_local::{TrackLocal, TrackLocalWriter, track_local_static_rtp::TrackLocalStaticRTP},
        track_remote::TrackRemote,
    },
};

const TIMESTAMP_URI: &str = "urn:rcof:same-host-frame-created-us";
#[derive(Clone, Debug, clap::Args)]
pub struct Options {
    #[arg(long)]
    pub id: String,
    /// Fixed UDP media port for the local link lab.
    #[arg(long, requires = "peer_relay")]
    pub media_port: Option<u16>,
    /// Replace all remote ICE candidate ports with this loopback relay port.
    #[arg(long, requires = "media_port")]
    pub peer_relay: Option<u16>,
    /// Adapt video from RTCP loss feedback, within the selected maximum quality.
    #[arg(long)]
    pub adaptive: bool,
    #[arg(long, default_value = "demo")]
    pub room: String,
    #[arg(long, default_value = "127.0.0.1:8790")]
    pub server: SocketAddr,
    #[arg(long, value_enum, default_value = "tone")]
    pub source: Source,
    #[arg(long, value_enum, default_value = "null")]
    pub sink: Sink,
    #[arg(long)]
    pub input_device: Option<String>,
    #[arg(long)]
    pub output_device: Option<String>,
    #[arg(long, default_value_t = 440.0)]
    pub tone_hz: f32,
    /// Override the selected audio profile's Opus bitrate.
    #[arg(long,value_parser=clap::value_parser!(u32).range(6000..=128000))]
    pub bitrate: Option<u32>,
    #[arg(long, value_enum, default_value = "standard")]
    pub audio_profile: audio::Profile,
    /// Opus encoding effort: lower uses less CPU, potentially reducing quality.
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u8).range(0..=10))]
    pub audio_complexity: u8,
    /// Keep encoding quiet audio instead of using the profile's silence compression.
    #[arg(long)]
    pub no_audio_dtx: bool,
    /// Stop after this many connected seconds; 0 means until quit/Ctrl-C.
    #[arg(long, default_value_t = 0)]
    pub duration: u64,
    #[arg(long,default_value_t=30,value_parser=clap::value_parser!(u64).range(1..=3600))]
    pub connect_timeout: u64,
    #[arg(long)]
    pub stats_file: Option<PathBuf>,
    #[arg(long)]
    pub no_stdin: bool,
    #[command(flatten)]
    pub video: video::Options,
}
struct Peer {
    pc: Arc<RTCPeerConnection>,
    track: Arc<TrackLocalStaticRTP>,
    sender: Arc<RTCRtpSender>,
    video_track: Option<Arc<TrackLocalStaticRTP>>,
    video_sender: Option<Arc<RTCRtpSender>>,
    states: mpsc::Receiver<RTCPeerConnectionState>,
    incoming: mpsc::Receiver<(Arc<TrackRemote>, Arc<RTCRtpReceiver>)>,
}
impl Peer {
    async fn new(id: &str, enable_video: bool, media_port: Option<u16>) -> Result<Self> {
        let mut media = MediaEngine::default();
        // SDP uses the standard Opus two-channel capability; the encoder sends mono.
        let codec = RTCRtpCodecCapability {
            mime_type: "audio/opus".into(),
            clock_rate: RATE,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=0;stereo=0".into(),
            ..Default::default()
        };
        media.register_codec(
            RTCRtpCodecParameters {
                capability: codec.clone(),
                payload_type: 111,
                ..Default::default()
            },
            RTPCodecType::Audio,
        )?;
        media.register_header_extension(
            RTCRtpHeaderExtensionCapability {
                uri: TIMESTAMP_URI.into(),
            },
            RTPCodecType::Audio,
            None,
        )?;
        let video_codec = RTCRtpCodecCapability {
            mime_type: "video/VP8".into(),
            clock_rate: 90_000,
            ..Default::default()
        };
        // Capture choice does not remove our ability to receive VP8.
        media.register_codec(
            RTCRtpCodecParameters {
                capability: video_codec.clone(),
                payload_type: 96,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
        media.register_header_extension(
            RTCRtpHeaderExtensionCapability {
                uri: video::TIME_URI.into(),
            },
            RTPCodecType::Video,
            None,
        )?;
        // On constrained links unbounded NACK retries can overwhelm fresh speech.
        // Use periodic VP8 keyframes for recovery and RTCP reports for adaptation.
        let registry = configure_rtcp_reports(Registry::new());
        let mut settings = SettingEngine::default();
        if let Some(port) = media_port {
            anyhow::ensure!(port > 0, "media port must be nonzero");
            settings.set_udp_network(webrtc::ice::udp_network::UDPNetwork::Ephemeral(
                webrtc::ice::udp_network::EphemeralUDP::new(port, port)?,
            ));
        }
        settings.set_network_types(vec![NetworkType::Udp4]);
        settings.set_include_loopback_candidate(true);
        settings.set_ip_filter(Box::new(move |ip| {
            if media_port.is_some() {
                ip == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            } else {
                ip.is_loopback()
            }
        }));
        settings.set_ice_multicast_dns_mode(MulticastDnsMode::Disabled);
        let api = APIBuilder::new()
            .with_media_engine(media)
            .with_interceptor_registry(registry)
            .with_setting_engine(settings)
            .build();
        // Empty ICE server list: no STUN, TURN, DNS, or cloud signaling.
        let pc = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await?);
        let (state_tx, states) = mpsc::channel(16);
        pc.on_peer_connection_state_change(Box::new(move |state| {
            let tx = state_tx.clone();
            Box::pin(async move {
                let _ = tx.send(state).await;
            })
        }));
        let (track_tx, incoming) = mpsc::channel(2);
        pc.on_track(Box::new(move |track, receiver, _| {
            let tx = track_tx.clone();
            Box::pin(async move {
                let _ = tx.send((track, receiver)).await;
            })
        }));
        let track = Arc::new(TrackLocalStaticRTP::new(codec, "audio".into(), id.into()));
        let sender = pc
            .add_track(track.clone() as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        let (video_track, video_sender) = if enable_video {
            let track = Arc::new(TrackLocalStaticRTP::new(
                video_codec,
                "video".into(),
                id.into(),
            ));
            let sender = pc
                .add_track(track.clone() as Arc<dyn TrackLocal + Send + Sync>)
                .await?;
            (Some(track), Some(sender))
        } else {
            pc.add_transceiver_from_kind(
                RTPCodecType::Video,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await?;
            (None, None)
        };
        Ok(Self {
            video_track,
            video_sender,
            pc,
            track,
            sender,
            states,
            incoming,
        })
    }
    async fn local_description(&self, offer: bool) -> Result<RTCSessionDescription> {
        let mut gathering = self.pc.gathering_complete_promise().await;
        let sdp = if offer {
            self.pc.create_offer(None).await?
        } else {
            self.pc.create_answer(None).await?
        };
        self.pc.set_local_description(sdp).await?;
        timeout(Duration::from_secs(5), gathering.recv())
            .await
            .context("loopback ICE gathering timed out")?;
        let description = self
            .pc
            .local_description()
            .await
            .context("missing local SDP")?;
        validate_sdp(&description.sdp)?;
        Ok(description)
    }
}

pub fn relay_sdp(sdp: &str, port: u16) -> Result<String> {
    anyhow::ensure!(port > 0, "relay port must be nonzero");
    validate_sdp(sdp)?;
    let mut lines = Vec::new();
    for line in sdp.lines() {
        if line.starts_with("a=candidate:") {
            let mut fields: Vec<String> = line.split_whitespace().map(str::to_string).collect();
            fields[4] = "127.0.0.1".into();
            fields[5] = port.to_string();
            lines.push(fields.join(" "));
        } else {
            lines.push(line.to_string());
        }
    }
    Ok(lines.join("\r\n") + "\r\n")
}

pub fn validate_sdp(sdp: &str) -> Result<()> {
    let mut candidates = 0;
    for line in sdp.lines().filter(|l| l.starts_with("a=candidate:")) {
        let fields: Vec<_> = line.split_whitespace().collect();
        let ip = fields
            .get(4)
            .context("malformed ICE candidate")?
            .parse::<std::net::IpAddr>()
            .context("P0 rejects DNS ICE candidates")?;
        if !ip.is_loopback()
            || fields.get(2).is_none_or(|v| !v.eq_ignore_ascii_case("udp"))
            || fields.get(7) != Some(&"host")
        {
            bail!("P0 rejects non-loopback/non-host/non-UDP ICE candidates");
        }
        candidates += 1;
    }
    if candidates == 0 {
        bail!("no loopback ICE candidates: this WebRTC stack must support same-Mac connections");
    }
    Ok(())
}

/// Small reorder window before declaring an Opus packet missing. Sequence arithmetic wraps.
#[derive(Default)]
struct AudioReorder {
    expected: Option<u16>,
    queued: Vec<(Packet, Instant)>,
}
impl AudioReorder {
    fn push(&mut self, packet: Packet, now: Instant) -> bool {
        let seq = packet.header.sequence_number;
        if self.expected.is_some_and(|e| seq.wrapping_sub(e) > 32767)
            || self
                .queued
                .iter()
                .any(|(p, _)| p.header.sequence_number == seq)
        {
            return false;
        }
        self.expected.get_or_insert(seq);
        self.queued.push((packet, now));
        true
    }
    fn pop(&mut self, now: Instant) -> Option<Packet> {
        let expected = self.expected?;
        let index = self
            .queued
            .iter()
            .position(|(p, _)| p.header.sequence_number == expected)
            .or_else(|| {
                if self.queued.len() >= 32
                    || self
                        .queued
                        .first()
                        .is_some_and(|(_, at)| now.duration_since(*at) >= Duration::from_millis(30))
                {
                    self.queued
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, (p, _))| p.header.sequence_number.wrapping_sub(expected))
                        .map(|(i, _)| i)
                } else {
                    None
                }
            })?;
        let (packet, _) = self.queued.remove(index);
        self.expected = Some(packet.header.sequence_number.wrapping_add(1));
        Some(packet)
    }
    async fn next(&mut self, track: &TrackRemote, metrics: &Metrics) -> Result<Packet> {
        loop {
            if let Some(packet) = self.pop(Instant::now()) {
                return Ok(packet);
            }
            tokio::select! {
                result=track.read_rtp()=>{let(packet,_)=result.context("remote audio track ended")?;if !self.push(packet,Instant::now()){add(&metrics.late_packets,1);}},
                _=tokio::time::sleep(Duration::from_millis(5)),if !self.queued.is_empty()=>{},
            }
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn enqueue_audio(
    pcm: &[f32],
    created: u64,
    synchronize: bool,
    output_rate: u32,
    resampler: &mut Resampler,
    queue: &crossbeam_queue::ArrayQueue<PlaybackSample>,
    metrics: &Metrics,
) {
    let mut output_index = 0u64;
    for sample in pcm {
        resampler.push(*sample, |value| {
            let play_at_us = if synchronize && created > 0 {
                created + video::PLAYOUT_US + output_index * 1_000_000 / output_rate as u64
            } else {
                0
            };
            output_index += 1;
            if queue
                .force_push(PlaybackSample { value, play_at_us })
                .is_some()
            {
                add(&metrics.playback_dropped, 1);
            }
        });
    }
    metrics
        .playback_peak
        .fetch_max(queue.len() as u64, Ordering::Relaxed);
}

async fn receive(
    track: Arc<TrackRemote>,
    receiver: Arc<RTCRtpReceiver>,
    queue: Arc<crossbeam_queue::ArrayQueue<PlaybackSample>>,
    output_rate: u32,
    sink: Sink,
    metrics: Arc<Metrics>,
    synchronize: bool,
) -> Result<()> {
    let ext = receiver
        .get_parameters()
        .await
        .header_extensions
        .iter()
        .find(|e| e.uri == TIMESTAMP_URI)
        .map(|e| e.id as u8);
    let mut decoder = opus::Decoder::new(RATE, opus::Channels::Mono)?;
    let mut buffer = [0.0f32; 5760];
    let mut resampler = Resampler::new(RATE, output_rate);
    let mut last_sequence: Option<u16> = None;
    let mut last_samples = FRAME;
    let mut reorder = AudioReorder::default();
    loop {
        let packet = reorder.next(&track, &metrics).await?;
        // Read the duration from the peer's Opus packet, not our local profile.
        let packet_samples = match decoder.get_nb_samples(&packet.payload) {
            Ok(n) if n > 0 && n <= buffer.len() => n,
            _ => {
                add(&metrics.decode_errors, 1);
                continue;
            }
        };
        let mut missing = 0u16;
        if let Some(last) = last_sequence {
            let step = packet.header.sequence_number.wrapping_sub(last);
            if step == 0 || step > 32768 {
                add(&metrics.late_packets, 1);
                continue;
            }
            if step > 1 {
                add(&metrics.gaps, (step - 1) as usize);
                missing = (step - 1).min((3 * FRAME / last_samples) as u16);
            }
        }
        last_sequence = Some(packet.header.sequence_number);
        add(&metrics.rx_payload, packet.payload.len());
        let mut frame_created = 0;
        if let Some(bytes) = ext.and_then(|id| packet.header.get_extension(id))
            && let Ok(raw) = <[u8; 8]>::try_from(bytes.as_ref())
            && let Some(age) = monotonic_us().checked_sub(u64::from_be_bytes(raw))
            && age < 10_000_000
        {
            frame_created = u64::from_be_bytes(raw);
            metrics.frame_to_decode.record(age);
            metrics.audio_age_us.store(age, Ordering::Relaxed);
        }
        // Conceal at most 60 ms; a long outage must not create a catch-up audio burst.
        for back in (1..=missing).rev() {
            let n = decoder.decode_float(&[], &mut buffer[..last_samples], false)?;
            add(&metrics.audio_concealed_frames, 1);
            if sink == Sink::Speaker {
                enqueue_audio(
                    &buffer[..n],
                    frame_created.saturating_sub(
                        back as u64 * last_samples as u64 * 1_000_000 / RATE as u64,
                    ),
                    synchronize,
                    output_rate,
                    &mut resampler,
                    &queue,
                    &metrics,
                );
            }
        }
        let started = Instant::now();
        let n = match decoder.decode_float(&packet.payload, &mut buffer, false) {
            Ok(n) => n,
            Err(_) => {
                add(&metrics.decode_errors, 1);
                continue;
            }
        };
        metrics
            .decode_time
            .record(started.elapsed().as_micros() as u64);
        last_samples = packet_samples;
        metrics.rx_packet_samples.store(n as u64, Ordering::Relaxed);
        add(&metrics.rx_samples, n);
        add(&metrics.rx_frames, 1);
        if buffer[..n].iter().any(|v| v.abs() > 0.001) {
            add(&metrics.rx_non_silent, 1);
        }
        if sink == Sink::Speaker {
            enqueue_audio(
                &buffer[..n],
                frame_created,
                synchronize,
                output_rate,
                &mut resampler,
                &queue,
                &metrics,
            );
        }
    }
}

pub async fn run(options: Options) -> Result<()> {
    let (commands, input) = mpsc::channel::<String>(8);
    if !options.no_stdin {
        // A detached std thread does not keep the Tokio blocking pool alive at shutdown.
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(s) => {
                        if commands.blocking_send(s).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
    run_controlled(options, input, Arc::new(CallView::default())).await
}

pub async fn run_controlled(
    options: Options,
    input: mpsc::Receiver<String>,
    view: Arc<CallView>,
) -> Result<()> {
    view.state("Opening devices");
    view.video.enabled.store(
        options.video.video != video::VideoSource::Off,
        Ordering::Relaxed,
    );
    if !options.tone_hz.is_finite() || !(20.0..=4000.0).contains(&options.tone_hz) {
        bail!("--tone-hz must be between 20 and 4000");
    }
    let client = signaling::Client::new(options.server)?;
    let metrics = Arc::new(Metrics::default());
    metrics
        .video_quality_level
        .store(options.video.quality as u64, Ordering::Relaxed);
    let audio = Audio::open(
        options.source,
        options.sink,
        options.input_device.as_deref(),
        options.output_device.as_deref(),
        metrics.clone(),
        options.video.video != video::VideoSource::Off,
    )?;
    view.video
        .quality
        .store(options.video.quality as u64, Ordering::Relaxed);
    let mut peer = Peer::new(
        &options.id,
        options.video.video != video::VideoSource::Off,
        options.media_port,
    )
    .await?;
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    let result = async {
        let token = client.join(&options.room, &options.id).await?;
        println!(
            "joined room={} id={} source={:?} sink={:?} video={:?}; commands: mute, unmute, stats, quit",
            options.room, options.id, options.source, options.sink, options.video.video
        );
        view.state("Waiting for peer");
        let result = session(
            &options,
            &client,
            &token,
            &mut peer,
            &audio,
            metrics.clone(),
            &mut tasks,
            input,
            view.clone(),
        )
        .await;
        if let Err(error) = client.leave(&token).await {
            eprintln!("session cleanup: {error:#}");
        }
        result
    }
    .await;
    update_transport_metrics(&peer.pc, &metrics).await;
    tasks.abort_all();
    let _ = timeout(Duration::from_secs(3), peer.pc.close()).await;
    while tasks.join_next().await.is_some() {}
    drop(audio);
    let outcome = match &result {
        Ok(reason) => reason.clone(),
        Err(error) => format!("error: {error:#}"),
    };
    let summary = metrics.summary(&options.id, started.elapsed().as_secs_f64(), &outcome);
    *view.summary.lock().unwrap() = Some(summary.clone());
    view.state(&outcome);
    view.ended.store(true, Ordering::Relaxed);
    view.video.clear();
    let json = serde_json::to_string_pretty(&summary)?;
    println!("FINAL_STATS {json}");
    if let Some(path) = &options.stats_file {
        std::fs::write(path, json)
            .with_context(|| format!("cannot write stats to {}", path.display()))?;
    }
    result.map(|_| ())
}

#[allow(clippy::too_many_arguments)]
async fn session(
    options: &Options,
    client: &signaling::Client,
    token: &str,
    peer: &mut Peer,
    audio: &Audio,
    metrics: Arc<Metrics>,
    tasks: &mut JoinSet<Result<()>>,
    mut input: mpsc::Receiver<String>,
    view: Arc<CallView>,
) -> Result<String> {
    let sender = peer.sender.clone();
    tasks.spawn(async move {
        loop {
            sender.read_rtcp().await?;
        }
    });
    let muted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut _audio_sender = None;
    let mut _audio_receiver = None;
    let mut offered = false;
    let mut had_peer = false;
    let mut connected = None;
    let mut remote_set = false;
    let mut receiving = false;
    let mut receiving_video = false;
    let joined = Instant::now();
    let mut disconnected = None;
    let mut last_audio_count = 0;
    let mut last_audio_at = Instant::now();
    let mut polling = Box::pin(poll_after(client, token, Duration::ZERO));
    let mut poll_clock = tokio::time::interval(Duration::from_millis(200));
    poll_clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut stats_clock = tokio::time::interval(Duration::from_secs(5));
    stats_clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok("interrupted".into()),
            Some(command) = input.recv() => match command.trim() {
                "mute" => {
                    muted.store(true, Ordering::Relaxed);
                    println!("muted");
                }
                "unmute" => {
                    muted.store(false, Ordering::Relaxed);
                    println!("unmuted");
                }
                "camera-off" => {
                    view.video.enabled.store(false,Ordering::Relaxed);
                    println!("camera off");
                }
                "camera-on" => {
                    view.video.enabled.store(options.video.video!=video::VideoSource::Off,Ordering::Relaxed);
                    println!("camera on");
                }
                "quality-tiny" => view.video.quality.store(video::Quality::Tiny as u64,Ordering::Relaxed),
                "quality-minimal" => view.video.quality.store(video::Quality::Minimal as u64,Ordering::Relaxed),
                "quality-low" => view.video.quality.store(video::Quality::Low as u64,Ordering::Relaxed),
                "quality-standard" => view.video.quality.store(video::Quality::Standard as u64,Ordering::Relaxed),
                "quit" | "leave" => return Ok("left".into()),
                "stats" => println!("{}", serde_json::to_string(
                    &metrics.summary(&options.id, joined.elapsed().as_secs_f64(), "running")
                )?),
                _ => println!("commands: mute, unmute, stats, quit"),
            },
            Some(state) = peer.states.recv() => {
                println!("connection: {state}");
                view.state(state.to_string());
                match state {
                    RTCPeerConnectionState::Connected => {
                        if connected.is_none() {
                            connected = Some(Instant::now());
                            let extension = peer.sender.get_parameters().await
                                .rtp_parameters.header_extensions.iter()
                                .find(|e| e.uri == TIMESTAMP_URI)
                                .map(|e| e.id as u8);
                            // Discard samples recorded while waiting for the other instance.
                            while audio.capture.pop().is_some() {}
                            let (guard, finished) = AudioWorker::start(send_audio(options.clone(), audio.capture.clone(), peer.track.clone(), extension, muted.clone(), metrics.clone()))?;
                            _audio_sender = Some(guard);
                            tasks.spawn(async move { finished.await.context("audio sender thread stopped")? });
                            if let (Some(track),Some(sender))=(&peer.video_track,&peer.video_sender) {
                                let ext=sender.get_parameters().await.rtp_parameters.header_extensions.iter().find(|e|e.uri==video::TIME_URI).map(|e|e.id as u8);
                                tasks.spawn(video::send(options.video.clone(),options.id.clone(),track.clone(),ext,view.video.clone(),metrics.clone()));
                                let sender=sender.clone();
                                tasks.spawn(video::feedback(sender,view.video.clone(),metrics.clone(),options.adaptive,options.video.quality));
                            }
                        }
                        disconnected = None;
                    }
                    RTCPeerConnectionState::Failed => bail!("media connection failed"),
                    RTCPeerConnectionState::Disconnected => {
                        disconnected = Some(Instant::now());
                    }
                    RTCPeerConnectionState::Closed => return Ok("connection closed".into()),
                    _ => {}
                }
            },
            Some((track, receiver)) = peer.incoming.recv() => {
                if track.kind()==RTPCodecType::Video {
                    if receiving_video {bail!("unexpected additional video track");}
                    receiving_video=true;
                    tasks.spawn(video::receive(track,receiver,options.video.ffmpeg.clone(),view.video.clone(),metrics.clone()));
                } else {
                    if receiving {bail!("unexpected additional audio track");}
                    receiving=true;
                    let (guard, finished) = AudioWorker::start(receive(track,receiver,audio.playback.clone(),audio.output_rate,options.sink,metrics.clone(),options.video.video!=video::VideoSource::Off))?;
                    _audio_receiver = Some(guard);
                    tasks.spawn(async move { finished.await.context("audio receiver thread stopped")? });
                }
            },
            Some(result) = tasks.join_next() => {
                result.context("media task panicked")??;
                bail!("media task unexpectedly stopped");
            },
            _ = poll_clock.tick() => {
                if connected.is_some() && disconnected.is_none() {
                    let count=get(&metrics.rx_frames);
                    if count!=last_audio_count {last_audio_count=count;last_audio_at=Instant::now();view.state("connected");}
                    else if last_audio_at.elapsed()>Duration::from_millis(1500) {view.state("Media interrupted — waiting for link");}
                }
                if options.duration > 0 && connected.is_some_and(|since: Instant| since.elapsed() >= Duration::from_secs(options.duration)) {
                    if get(&metrics.rx_frames) == 0 { bail!("no audio received during the call"); }
                    return Ok("duration reached".into());
                }
                if connected.is_none()
                    && joined.elapsed() > Duration::from_secs(options.connect_timeout)
                {
                    bail!("connection timeout: start a second instance with another --id in the same room");
                }
                if disconnected.is_some_and(|since: Instant| since.elapsed() > Duration::from_secs(12)) {
                    bail!("peer disconnected for more than twelve seconds");
                }
                if options.source == Source::Mic
                    && joined.elapsed() > Duration::from_secs(3)
                    && get(&metrics.captured_samples) == 0
                {
                    bail!("microphone produced no samples; check macOS permission and device selection");
                }
                if get(&metrics.device_errors) > 0 {
                    bail!("audio device stopped; check device connection and microphone permissions");
                }
            },
            poll = &mut polling => {
                let poll = poll?;
                polling.set(poll_after(client, token, Duration::from_millis(200)));
                if poll.peer.is_none() && had_peer {
                    return Ok("peer left".into());
                }
                if let Some(other) = poll.peer {
                    had_peer = true;
                    // Both peers use the same ordering rule, so only one creates an offer.
                    if !offered && !remote_set && options.id < other {
                        let offer = peer.local_description(true).await?;
                        client.send(token, &offer).await?;
                        offered = true;
                        println!("offered loopback media to {other}");
                    }
                }
                for mut sdp in poll.messages {
                    validate_sdp(&sdp.sdp)?;
                    if let Some(port)=options.peer_relay {sdp.sdp=relay_sdp(&sdp.sdp,port)?;}
                    if remote_set {
                        bail!("unexpected repeated session description");
                    }
                    let kind = sdp.sdp_type;
                    if (kind == RTCSdpType::Offer && offered)
                        || (kind == RTCSdpType::Answer && !offered)
                        || !matches!(kind, RTCSdpType::Offer | RTCSdpType::Answer)
                    {
                        bail!("unexpected signaling description type");
                    }
                    peer.pc.set_remote_description(sdp).await?;
                    remote_set = true;
                    if kind == RTCSdpType::Offer {
                        let answer = peer.local_description(false).await?;
                        client.send(token, &answer).await?;
                    }
                }
            },
            _ = stats_clock.tick() => {
                update_transport_metrics(&peer.pc, &metrics).await;
                *view.summary.lock().unwrap()=Some(metrics.summary(&options.id,joined.elapsed().as_secs_f64(),"running"));
                println!("STATS {}", serde_json::to_string(
                    &metrics.summary(&options.id, joined.elapsed().as_secs_f64(), "running")
                )?);
            },
        }
    }
}

async fn poll_after(
    client: &signaling::Client,
    token: &str,
    delay: Duration,
) -> Result<signaling::Poll> {
    tokio::time::sleep(delay).await;
    client.poll(token).await
}

// Own the audio scheduler independently of CPU-heavy video work on Tokio workers.
struct AudioWorker {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl AudioWorker {
    fn start(
        future: impl std::future::Future<Output = Result<()>> + Send + 'static,
    ) -> Result<(Self, tokio::sync::oneshot::Receiver<Result<()>>)> {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (done, finished) = tokio::sync::oneshot::channel();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let thread = std::thread::Builder::new()
            .name("rcof-audio".into())
            .spawn(move || {
                let result = runtime.block_on(async move {
                    tokio::select! {
                        result = future => result,
                        _ = stopped => Ok(()),
                    }
                });
                let _ = done.send(result);
            })?;
        Ok((
            Self {
                stop: Some(stop),
                thread: Some(thread),
            },
            finished,
        ))
    }
}
impl Drop for AudioWorker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// Sending audio must not wait for signaling HTTP requests or statistics collection.
async fn send_audio(
    options: Options,
    capture: Arc<crossbeam_queue::ArrayQueue<f32>>,
    track: Arc<TrackLocalStaticRTP>,
    extension: Option<u8>,
    muted: Arc<std::sync::atomic::AtomicBool>,
    metrics: Arc<Metrics>,
) -> Result<()> {
    let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)?;
    let packet_ms = options.audio_profile.packet_ms();
    let samples = (RATE as u64 * packet_ms / 1000) as usize;
    let bitrate = options.bitrate.unwrap_or(options.audio_profile.bitrate());
    encoder.set_bitrate(opus::Bitrate::Bits(bitrate as i32))?;
    encoder.set_dtx(options.audio_profile.dtx() && !options.no_audio_dtx)?;
    encoder.set_complexity(options.audio_complexity as i32)?;
    metrics
        .audio_complexity
        .store(options.audio_complexity as u64, Ordering::Relaxed);
    metrics.audio_packet_ms.store(packet_ms, Ordering::Relaxed);
    metrics
        .audio_target_bps
        .store(bitrate as u64, Ordering::Relaxed);
    eprintln!(
        "AUDIO_CONFIG profile={:?} packet_ms={} bitrate={} dtx={}",
        options.audio_profile,
        packet_ms,
        bitrate,
        options.audio_profile.dtx() && !options.no_audio_dtx
    );
    let mut pcm = vec![0.0f32; samples];
    let mut encoded = [0u8; 4000];
    let mut phase = 0.0;
    let mut sequence = 0u16;
    let mut timestamp = 0u32;
    let mut capture_ready = options.source != Source::Mic;
    let mut frame_clock = tokio::time::interval(Duration::from_millis(packet_ms));
    // Catch up short scheduler stalls instead of silently omitting PCM frames.
    frame_clock.set_missed_tick_behavior(MissedTickBehavior::Burst);
    loop {
        let tick = frame_clock.tick().await;
        // A long outage is not an invitation to queue an unlimited catch-up burst.
        if tick.elapsed() > Duration::from_millis(100) {
            frame_clock.reset();
        }
        // Device callbacks arrive in chunks. Reserve one packet plus 20 ms
        // before starting, rather than inserting startup silence.
        if !capture_ready {
            if capture.len() < samples + FRAME {
                continue;
            }
            capture_ready = true;
        }
        let created = monotonic_us();
        if options.source == Source::Tone {
            audio::tone(&mut pcm, &mut phase, options.tone_hz);
        } else if options.source == Source::Silence {
            pcm.fill(0.0);
        } else {
            for sample in &mut pcm {
                *sample = capture.pop().unwrap_or_else(|| {
                    add(&metrics.capture_missing, 1);
                    0.0
                });
            }
        }
        let rms = (pcm.iter().map(|v| v * v).sum::<f32>() / samples as f32).sqrt();
        metrics
            .source_rms_ppm
            .store((rms * 1_000_000.0) as u64, Ordering::Relaxed);
        if muted.load(Ordering::Relaxed) {
            pcm.fill(0.0);
        }
        let begin = Instant::now();
        let n = encoder.encode_float(&pcm, &mut encoded)?;
        metrics
            .encode_time
            .record(begin.elapsed().as_micros() as u64);
        let mut packet = Packet {
            header: Header {
                version: 2,
                sequence_number: sequence,
                timestamp,
                ..Default::default()
            },
            payload: Bytes::copy_from_slice(&encoded[..n]),
        };
        if let Some(id) = extension {
            packet
                .header
                .set_extension(id, Bytes::copy_from_slice(&created.to_be_bytes()))?;
        }
        timeout(Duration::from_millis(200), track.write_rtp(&packet))
            .await
            .context("audio send stalled")??;
        sequence = sequence.wrapping_add(1);
        timestamp = timestamp.wrapping_add(samples as u32);
        add(&metrics.tx_samples, samples);
        add(&metrics.tx_frames, 1);
        add(&metrics.tx_payload, n);
    }
}

async fn update_transport_metrics(pc: &RTCPeerConnection, metrics: &Metrics) {
    for report in pc.get_stats().await.reports.values() {
        if let webrtc::stats::StatsReportType::Transport(stats) = report {
            metrics
                .transport_tx
                .store(stats.bytes_sent as u64, Ordering::Relaxed);
            metrics
                .transport_rx
                .store(stats.bytes_received as u64, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reorder_waits_for_missing_audio_and_handles_wrap() {
        fn packet(seq: u16) -> Packet {
            Packet {
                header: Header {
                    sequence_number: seq,
                    ..Default::default()
                },
                ..Default::default()
            }
        }
        let now = Instant::now();
        let mut q = AudioReorder::default();
        q.push(packet(65535), now);
        assert_eq!(q.pop(now).unwrap().header.sequence_number, 65535);
        q.push(packet(1), now);
        assert!(q.pop(now).is_none());
        q.push(packet(0), now);
        assert_eq!(q.pop(now).unwrap().header.sequence_number, 0);
        assert_eq!(q.pop(now).unwrap().header.sequence_number, 1);
        assert!(!q.push(packet(0), now));
        q.push(packet(3), now);
        assert!(q.pop(now + Duration::from_millis(29)).is_none());
        assert_eq!(
            q.pop(now + Duration::from_millis(31))
                .unwrap()
                .header
                .sequence_number,
            3
        );
    }
    #[test]
    fn relay_replaces_every_candidate() {
        let source = "a=candidate:1 1 udp 1 127.0.0.1 1000 typ host\r\na=candidate:2 1 udp 1 127.0.0.2 1001 typ host\r\n";
        let mapped = relay_sdp(source, 9999).unwrap();
        assert_eq!(mapped.matches("127.0.0.1 9999").count(), 2);
        assert!(!mapped.contains("1000"));
        assert!(!mapped.contains("1001"));
        assert!(relay_sdp(source, 0).is_err());
    }
    #[test]
    fn rejects_external_ice_candidates() {
        assert!(validate_sdp("a=candidate:1 1 udp 1 127.0.0.1 9999 typ host\r\n").is_ok());
        for s in [
            "a=candidate:1 1 udp 1 192.168.1.2 9999 typ host",
            "a=candidate:1 1 udp 1 host.local 9999 typ host",
            "a=candidate:1 1 tcp 1 127.0.0.1 9999 typ host",
            "v=0",
        ] {
            assert!(validate_sdp(s).is_err());
        }
    }
}
