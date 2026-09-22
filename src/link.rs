//! Loopback datagram link lab. Both directions carry ICE, DTLS, SRTP and RTCP.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::net::UdpSocket;

#[derive(Clone, Debug, clap::Args)]
pub struct Options {
    #[arg(long)]
    pub config: PathBuf,
    #[arg(long)]
    pub report: PathBuf,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Phase {
    pub at_s: f64,
    /// Per-direction IPv4 + UDP + encrypted payload bitrate. Zero means unlimited.
    #[serde(default)]
    pub kbps: u32,
    #[serde(default)]
    pub delay_ms: u32,
    #[serde(default)]
    pub jitter_ms: u32,
    #[serde(default)]
    pub loss_percent: f64,
    #[serde(default)]
    pub outage: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    pub a: SocketAddr,
    pub b: SocketAddr,
    pub relay_a: SocketAddr,
    pub relay_b: SocketAddr,
    pub duration_s: u64,
    pub seed: u64,
    pub phases: Vec<Phase>,
    #[serde(default = "queue_default")]
    pub queue_bytes: usize,
    #[serde(default = "age_default")]
    pub queue_ms: u64,
}
fn queue_default() -> usize {
    32768
}
fn age_default() -> u64 {
    150
}
impl Config {
    fn validate(&self) -> Result<()> {
        for address in [self.a, self.b, self.relay_a, self.relay_b] {
            ensure!(
                address.ip().is_loopback() && address.is_ipv4() && address.port() > 0,
                "link endpoints must be IPv4 loopback with nonzero ports"
            );
        }
        let mut addresses = vec![self.a, self.b, self.relay_a, self.relay_b];
        addresses.sort();
        addresses.dedup();
        ensure!(addresses.len() == 4, "four distinct endpoints required");
        ensure!(
            self.duration_s <= 3600,
            "duration must be 0 (until interrupted) or 1..3600 seconds"
        );
        ensure!(
            (1500..=1_048_576).contains(&self.queue_bytes) && (1..=2000).contains(&self.queue_ms),
            "invalid queue bounds"
        );
        ensure!(
            !self.phases.is_empty() && self.phases[0].at_s == 0.0,
            "first phase must start at zero"
        );
        for (i, p) in self.phases.iter().enumerate() {
            ensure!(
                p.at_s.is_finite()
                    && p.at_s >= 0.0
                    && (self.duration_s == 0 || p.at_s < self.duration_s as f64),
                "invalid phase time"
            );
            ensure!(
                i == 0 || p.at_s > self.phases[i - 1].at_s,
                "phases must be ordered"
            );
            ensure!(
                p.loss_percent.is_finite()
                    && (0.0..=100.0).contains(&p.loss_percent)
                    && p.delay_ms <= 2000
                    && p.jitter_ms <= 2000,
                "invalid impairment"
            );
        }
        Ok(())
    }
}
#[derive(Default, Debug, Serialize, Clone)]
pub struct Counts {
    pub received_packets: u64,
    pub delivered_packets: u64,
    pub received_ip_bytes: u64,
    pub delivered_ip_bytes: u64,
    pub audio_delivered: u64,
    pub video_delivered: u64,
    pub control_delivered: u64,
    pub loss_drops: u64,
    pub outage_drops: u64,
    pub queue_drops: u64,
    pub stale_drops: u64,
    pub unexpected_sources: u64,
    pub peak_queue_bytes: usize,
}
#[derive(Serialize)]
struct Snapshot {
    elapsed_s: f64,
    phase: usize,
    a_to_b: Counts,
    b_to_a: Counts,
}
#[derive(Serialize)]
struct Report {
    config: Config,
    a_to_b: Counts,
    b_to_a: Counts,
    snapshots: VecDeque<Snapshot>,
}
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Audio,
    Video,
    Control,
}
fn kind(data: &[u8]) -> Kind {
    if data.len() >= 12 && data[0] >> 6 == 2 {
        // SRTP leaves the RTP header visible. Keep RTCP separate before masking M.
        if (192..=223).contains(&data[1]) {
            return Kind::Control;
        }
        match data[1] & 127 {
            111 => Kind::Audio,
            96 => Kind::Video,
            _ => Kind::Control,
        }
    } else {
        Kind::Control
    }
}
struct Packet {
    data: Vec<u8>,
    kind: Kind,
    arrived: Instant,
    due: Instant,
}
impl Packet {
    fn size(&self) -> usize {
        self.data.len() + 28
    }
}
struct Direction {
    high: VecDeque<Packet>,
    low: VecDeque<Packet>,
    flying: Vec<Packet>,
    bytes: usize,
    free: Instant,
    rng: u64,
    counts: Counts,
}
impl Direction {
    fn new(seed: u64) -> Self {
        Self {
            high: VecDeque::new(),
            low: VecDeque::new(),
            flying: Vec::new(),
            bytes: 0,
            free: Instant::now(),
            rng: seed.max(1),
            counts: Counts::default(),
        }
    }
    fn random(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / ((1u64 << 53) as f64)
    }
    fn enqueue(&mut self, data: &[u8], phase: &Phase, cfg: &Config, now: Instant) {
        self.counts.received_packets += 1;
        self.counts.received_ip_bytes += (data.len() + 28) as u64;
        if phase.outage {
            self.counts.outage_drops += 1;
            return;
        }
        if self.random() * 100.0 < phase.loss_percent {
            self.counts.loss_drops += 1;
            return;
        }
        let k = kind(data);
        let size = data.len() + 28;
        // Audio/control may evict queued video, never already-serialized packets.
        while self.bytes + size > cfg.queue_bytes && k != Kind::Video {
            if let Some(p) = self.low.pop_back() {
                self.bytes -= p.size();
                self.counts.queue_drops += 1;
            } else {
                break;
            }
        }
        let limit = if k == Kind::Video {
            cfg.queue_bytes * 4 / 5
        } else {
            cfg.queue_bytes
        };
        if self.bytes + size > limit {
            self.counts.queue_drops += 1;
            return;
        }
        let jitter = (self.random() * 2.0 - 1.0) * phase.jitter_ms as f64;
        let due =
            now + Duration::from_secs_f64(((phase.delay_ms as f64 + jitter).max(0.0)) / 1000.0);
        let packet = Packet {
            data: data.to_vec(),
            kind: k,
            arrived: now,
            due,
        };
        if k == Kind::Video {
            self.low.push_back(packet);
        } else {
            self.high.push_back(packet);
        }
        self.bytes += size;
        self.counts.peak_queue_bytes = self.counts.peak_queue_bytes.max(self.bytes);
    }
    fn flush_outage(&mut self, now: Instant) {
        self.counts.outage_drops += (self.high.len() + self.low.len() + self.flying.len()) as u64;
        self.high.clear();
        self.low.clear();
        self.flying.clear();
        self.bytes = 0;
        self.free = now;
    }
    async fn pump(
        &mut self,
        socket: &UdpSocket,
        to: SocketAddr,
        p: &Phase,
        cfg: &Config,
        now: Instant,
    ) -> Result<()> {
        if p.outage {
            self.flush_outage(now);
            return Ok(());
        }
        // Bound queue residence separately from propagation delay.
        for q in [&mut self.high, &mut self.low] {
            while q.front().is_some_and(|v| {
                now.duration_since(v.arrived) > Duration::from_millis(cfg.queue_ms)
            }) {
                let v = q.pop_front().unwrap();
                self.bytes -= v.size();
                self.counts.stale_drops += 1;
            }
        }
        while self.free <= now {
            let Some(mut packet) = self.high.pop_front().or_else(|| self.low.pop_front()) else {
                break;
            };
            let serialization = if p.kbps == 0 {
                Duration::ZERO
            } else {
                Duration::from_secs_f64(packet.size() as f64 * 8.0 / (p.kbps as f64 * 1000.0))
            };
            // Never borrow tokens from idle time or release a rate-limited startup burst.
            self.free = now + serialization;
            packet.due = self.free + packet.due.duration_since(packet.arrived);
            self.flying.push(packet);
            if p.kbps > 0 {
                break;
            }
        }
        let mut i = 0;
        while i < self.flying.len() {
            if self.flying[i].due <= now {
                let packet = self.flying.remove(i);
                self.bytes -= packet.size();
                socket.send_to(&packet.data, to).await?;
                self.counts.delivered_packets += 1;
                self.counts.delivered_ip_bytes += packet.size() as u64;
                match packet.kind {
                    Kind::Audio => self.counts.audio_delivered += 1,
                    Kind::Video => self.counts.video_delivered += 1,
                    Kind::Control => self.counts.control_delivered += 1,
                }
            } else {
                i += 1;
            }
        }
        Ok(())
    }
}
pub async fn run(options: Options) -> Result<()> {
    let cfg: Config = serde_json::from_slice(&std::fs::read(options.config)?)?;
    cfg.validate()?;
    let ra = UdpSocket::bind(cfg.relay_a).await?;
    let rb = UdpSocket::bind(cfg.relay_b).await?;
    let mut ab = Direction::new(cfg.seed);
    let mut ba = Direction::new(cfg.seed.wrapping_add(0x9e3779b9));
    let start = Instant::now();
    let mut clock = tokio::time::interval(Duration::from_millis(1));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut a_buf = vec![0; 65536];
    let mut b_buf = vec![0; 65536];
    let mut snapshots = VecDeque::new();
    let mut last_second = 0;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    println!("LINK_READY {} {}", cfg.relay_a, cfg.relay_b);
    loop {
        let phase = cfg
            .phases
            .iter()
            .rposition(|p| p.at_s <= start.elapsed().as_secs_f64())
            .unwrap_or(0);
        let p = &cfg.phases[phase];
        tokio::select! {
            _=&mut shutdown=>break,
            r=rb.recv_from(&mut a_buf)=>{let(n,from)=r?;if from==cfg.a {ab.enqueue(&a_buf[..n],p,&cfg,Instant::now());}else{ab.counts.unexpected_sources+=1;eprintln!("ignored A→B source {from}");}},
            r=ra.recv_from(&mut b_buf)=>{let(n,from)=r?;if from==cfg.b {ba.enqueue(&b_buf[..n],p,&cfg,Instant::now());}else{ba.counts.unexpected_sources+=1;eprintln!("ignored B→A source {from}");}},
            _=clock.tick()=>{
                let now=Instant::now();ab.pump(&ra,cfg.b,p,&cfg,now).await?;ba.pump(&rb,cfg.a,p,&cfg,now).await?;
                let second=start.elapsed().as_secs();
                if second!=last_second {last_second=second;if snapshots.len() == 3600 {snapshots.pop_front();}snapshots.push_back(Snapshot{elapsed_s:start.elapsed().as_secs_f64(),phase,a_to_b:ab.counts.clone(),b_to_a:ba.counts.clone()});}
                if cfg.duration_s > 0 && second>=cfg.duration_s {break;}
            }
        }
    }
    let report = Report {
        config: cfg,
        a_to_b: ab.counts,
        b_to_a: ba.counts,
        snapshots,
    };
    std::fs::write(options.report, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_config_allows_phases_but_timed_config_checks_deadline() {
        let mut config: Config = serde_json::from_str(
            r#"{
            "a":"127.0.0.1:1", "b":"127.0.0.1:2",
            "relay_a":"127.0.0.1:3", "relay_b":"127.0.0.1:4",
            "duration_s":0, "seed":42,
            "phases":[{"at_s":0},{"at_s":30,"kbps":128}]
        }"#,
        )
        .unwrap();
        assert!(config.validate().is_ok());
        config.duration_s = 20;
        assert!(config.validate().is_err());
        config.duration_s = 36;
        assert!(config.validate().is_ok());
    }
    #[tokio::test]
    async fn shaper_serializes_and_prioritizes_audio() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let config = Config {
            a: "127.0.0.1:1".parse().unwrap(),
            b: "127.0.0.1:2".parse().unwrap(),
            relay_a: "127.0.0.1:3".parse().unwrap(),
            relay_b: "127.0.0.1:4".parse().unwrap(),
            duration_s: 10,
            seed: 42,
            phases: vec![],
            queue_bytes: 4096,
            queue_ms: 100,
        };
        let phase = Phase {
            at_s: 0.0,
            kbps: 128,
            delay_ms: 0,
            jitter_ms: 0,
            loss_percent: 0.0,
            outage: false,
        };
        let mut direction = Direction::new(42);
        let now = Instant::now();
        let mut payload = vec![0; 100];
        payload[0] = 0x80;
        payload[1] = 96;
        direction.enqueue(&payload, &phase, &config, now);
        payload[1] = 111;
        direction.enqueue(&payload, &phase, &config, now);
        direction
            .pump(
                &socket,
                receiver.local_addr().unwrap(),
                &phase,
                &config,
                now,
            )
            .await
            .unwrap();
        assert_eq!(direction.flying.len(), 1);
        assert!(direction.flying[0].kind == Kind::Audio);
        assert_eq!(direction.counts.delivered_packets, 0);
        direction
            .pump(
                &socket,
                receiver.local_addr().unwrap(),
                &phase,
                &config,
                now + Duration::from_millis(7),
            )
            .await
            .unwrap();
        assert_eq!(direction.counts.delivered_packets, 0);
        direction
            .pump(
                &socket,
                receiver.local_addr().unwrap(),
                &phase,
                &config,
                now + Duration::from_millis(8),
            )
            .await
            .unwrap();
        assert_eq!(direction.counts.audio_delivered, 1);
        assert_eq!(direction.counts.video_delivered, 0);
        direction.flush_outage(now);
        assert_eq!(direction.bytes, 0);
        assert_eq!(direction.counts.outage_drops, 1);
    }
    #[test]
    fn seeded_loss_is_repeatable_and_queue_is_bounded() {
        let cfg = Config {
            a: "127.0.0.1:1".parse().unwrap(),
            b: "127.0.0.1:2".parse().unwrap(),
            relay_a: "127.0.0.1:3".parse().unwrap(),
            relay_b: "127.0.0.1:4".parse().unwrap(),
            duration_s: 10,
            seed: 42,
            phases: vec![Phase {
                at_s: 0.0,
                kbps: 128,
                delay_ms: 50,
                jitter_ms: 10,
                loss_percent: 10.0,
                outage: false,
            }],
            queue_bytes: 4096,
            queue_ms: 100,
        };
        let mut a = Direction::new(42);
        let mut b = Direction::new(42);
        let mut payload = vec![0; 1000];
        payload[0] = 0x80;
        payload[1] = 96;
        for _ in 0..100 {
            a.enqueue(&payload, &cfg.phases[0], &cfg, Instant::now());
            b.enqueue(&payload, &cfg.phases[0], &cfg, Instant::now());
        }
        assert_eq!(a.counts.loss_drops, b.counts.loss_drops);
        assert!(a.counts.loss_drops > 0);
        assert!(a.bytes <= 4096);
        assert!(a.counts.queue_drops > 0);
        a.flush_outage(Instant::now());
        assert_eq!(a.bytes, 0);
    }
}
