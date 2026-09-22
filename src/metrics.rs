use serde::Serialize;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

pub fn add(value: &AtomicU64, n: usize) {
    value.fetch_add(n as u64, Ordering::Relaxed);
}
pub fn get(value: &AtomicU64) -> u64 {
    value.load(Ordering::Relaxed)
}

// Fixed-size histogram: memory use does not grow with call duration.
#[derive(Default)]
pub struct Timing(Mutex<[u64; 32]>);
impl Timing {
    pub fn record(&self, micros: u64) {
        let bucket = if micros == 0 {
            0
        } else {
            (64 - micros.leading_zeros()) as usize
        };
        self.0.lock().unwrap()[bucket.min(31)] += 1;
    }
    pub fn snapshot(&self) -> serde_json::Value {
        let bins = self.0.lock().unwrap();
        let count: u64 = bins.iter().sum();
        let quantile = |fraction: f64| -> Option<u64> {
            if count == 0 {
                return None;
            }
            let target = (count as f64 * fraction).ceil() as u64;
            let mut seen = 0;
            for (i, n) in bins.iter().enumerate() {
                seen += n;
                if seen >= target {
                    return Some(if i == 0 { 0 } else { (1u64 << i) - 1 });
                }
            }
            None
        };
        serde_json::json!({"samples":count,"p50_upper_us":quantile(0.5),"p95_upper_us":quantile(0.95)})
    }
}

#[derive(Default)]
pub struct Metrics {
    pub tx_frames: AtomicU64,
    pub tx_samples: AtomicU64,
    pub rx_samples: AtomicU64,
    pub rx_packet_samples: AtomicU64,
    pub audio_packet_ms: AtomicU64,
    pub audio_target_bps: AtomicU64,
    pub audio_complexity: AtomicU64,
    pub video_tx_frames: AtomicU64,
    pub video_rx_frames: AtomicU64,
    pub video_tx_bytes: AtomicU64,
    pub video_rx_bytes: AtomicU64,
    pub video_backpressure: AtomicU64,
    pub video_age_us: AtomicU64,
    pub video_loss_fraction: AtomicU64,
    pub video_quality_level: AtomicU64,
    pub video_quality_changes: AtomicU64,
    pub audio_age_us: AtomicU64,
    pub audio_concealed_frames: AtomicU64,
    pub video_encode: Timing,
    pub video_ready_age: Timing,
    pub audio_late_samples: AtomicU64,
    pub video_frame_to_decode: Timing,
    pub av_decode_skew: Timing,
    pub transport_tx: AtomicU64,
    pub transport_rx: AtomicU64,
    pub source_rms_ppm: AtomicU64,
    pub captured_samples: AtomicU64,
    pub rx_frames: AtomicU64,
    pub tx_payload: AtomicU64,
    pub rx_payload: AtomicU64,
    pub rx_non_silent: AtomicU64,
    pub capture_dropped: AtomicU64,
    pub capture_missing: AtomicU64,
    pub playback_dropped: AtomicU64,
    pub playback_missing: AtomicU64,
    pub playback_callback_peak: AtomicU64,
    pub playback_lead_peak: AtomicU64,
    pub capture_peak: AtomicU64,
    pub playback_peak: AtomicU64,
    pub gaps: AtomicU64,
    pub late_packets: AtomicU64,
    pub decode_errors: AtomicU64,
    pub device_errors: AtomicU64,
    pub encode_time: Timing,
    pub decode_time: Timing,
    pub frame_to_decode: Timing,
}

#[derive(Clone, Serialize)]
pub struct Summary {
    pub id: String,
    pub elapsed_seconds: f64,
    pub tx_frames: u64,
    pub tx_audio_samples: u64,
    pub rx_audio_samples: u64,
    pub audio_packet_ms: u64,
    pub audio_target_bps: u64,
    pub audio_complexity: u64,
    pub video_tx_frames: u64,
    pub video_loss_fraction: u64,
    pub video_quality_level: u64,
    pub video_quality_changes: u64,
    pub video_rx_frames: u64,
    pub video_tx_bytes: u64,
    pub video_rx_bytes: u64,
    pub video_tx_kbps: f64,
    pub video_rx_kbps: f64,
    pub video_backpressure_events: u64,
    pub video_encode: serde_json::Value,
    pub video_ready_age: serde_json::Value,
    pub audio_late_samples: u64,
    pub audio_concealed_frames: u64,
    pub video_frame_to_decode: serde_json::Value,
    pub av_decode_skew: serde_json::Value,
    pub transport_tx_bytes: u64,
    pub transport_rx_bytes: u64,
    pub transport_tx_kbps: f64,
    pub transport_rx_kbps: f64,
    pub source_rms_dbfs: Option<f64>,
    pub captured_samples: u64,
    pub rx_frames: u64,
    pub tx_opus_bytes: u64,
    pub rx_opus_bytes: u64,
    pub tx_opus_kbps: f64,
    pub rx_opus_kbps: f64,
    pub rx_non_silent_frames: u64,
    pub capture_dropped_samples: u64,
    pub capture_missing_samples: u64,
    pub playback_dropped_samples: u64,
    pub playback_missing_samples: u64,
    pub playback_callback_peak_samples: u64,
    pub playback_lead_peak_us: u64,
    pub capture_peak_samples: u64,
    pub playback_peak_samples: u64,
    pub sequence_gaps: u64,
    pub late_packets: u64,
    pub decode_errors: u64,
    pub device_errors: u64,
    pub encode: serde_json::Value,
    pub decode: serde_json::Value,
    pub frame_to_decode: serde_json::Value,
    pub outcome: String,
}
impl Metrics {
    pub fn summary(&self, id: &str, seconds: f64, outcome: &str) -> Summary {
        Summary {
            id: id.into(),
            elapsed_seconds: seconds,
            tx_frames: get(&self.tx_frames),
            tx_audio_samples: get(&self.tx_samples),
            rx_audio_samples: get(&self.rx_samples),
            audio_packet_ms: get(&self.audio_packet_ms),
            audio_target_bps: get(&self.audio_target_bps),
            audio_complexity: get(&self.audio_complexity),
            video_loss_fraction: get(&self.video_loss_fraction),
            video_quality_level: get(&self.video_quality_level),
            video_quality_changes: get(&self.video_quality_changes),
            video_tx_frames: get(&self.video_tx_frames),
            video_rx_frames: get(&self.video_rx_frames),
            video_tx_bytes: get(&self.video_tx_bytes),
            video_rx_bytes: get(&self.video_rx_bytes),
            video_tx_kbps: get(&self.video_tx_bytes) as f64 * 0.008 / seconds.max(0.001),
            video_rx_kbps: get(&self.video_rx_bytes) as f64 * 0.008 / seconds.max(0.001),
            video_backpressure_events: get(&self.video_backpressure),
            video_encode: self.video_encode.snapshot(),
            video_ready_age: self.video_ready_age.snapshot(),
            audio_concealed_frames: get(&self.audio_concealed_frames),
            audio_late_samples: get(&self.audio_late_samples),
            video_frame_to_decode: self.video_frame_to_decode.snapshot(),
            av_decode_skew: self.av_decode_skew.snapshot(),
            transport_tx_bytes: get(&self.transport_tx),
            transport_rx_bytes: get(&self.transport_rx),
            transport_tx_kbps: get(&self.transport_tx) as f64 * 0.008 / seconds.max(0.001),
            transport_rx_kbps: get(&self.transport_rx) as f64 * 0.008 / seconds.max(0.001),
            source_rms_dbfs: (get(&self.source_rms_ppm) > 0)
                .then(|| 20.0 * (get(&self.source_rms_ppm) as f64 / 1_000_000.0).log10()),
            captured_samples: get(&self.captured_samples),
            rx_frames: get(&self.rx_frames),
            tx_opus_bytes: get(&self.tx_payload),
            rx_opus_bytes: get(&self.rx_payload),
            tx_opus_kbps: get(&self.tx_payload) as f64 * 0.008 / seconds.max(0.001),
            rx_opus_kbps: get(&self.rx_payload) as f64 * 0.008 / seconds.max(0.001),
            rx_non_silent_frames: get(&self.rx_non_silent),
            capture_dropped_samples: get(&self.capture_dropped),
            capture_missing_samples: get(&self.capture_missing),
            playback_dropped_samples: get(&self.playback_dropped),
            playback_missing_samples: get(&self.playback_missing),
            playback_callback_peak_samples: get(&self.playback_callback_peak),
            playback_lead_peak_us: get(&self.playback_lead_peak),
            capture_peak_samples: get(&self.capture_peak),
            playback_peak_samples: get(&self.playback_peak),
            sequence_gaps: get(&self.gaps),
            late_packets: get(&self.late_packets),
            decode_errors: get(&self.decode_errors),
            device_errors: get(&self.device_errors),
            encode: self.encode_time.snapshot(),
            decode: self.decode_time.snapshot(),
            frame_to_decode: self.frame_to_decode.snapshot(),
            outcome: outcome.into(),
        }
    }
}

/// CLOCK_MONOTONIC is shared by both processes on this Mac. Never use this metric
/// across machines. It measures frame assembly to decoder completion, not acoustics.
pub fn monotonic_us() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: t points to valid writable storage and CLOCK_MONOTONIC is supported.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    assert_eq!(result, 0);
    t.tv_sec as u64 * 1_000_000 + t.tv_nsec as u64 / 1_000
}
