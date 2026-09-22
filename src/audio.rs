use crate::metrics::{Metrics, add, monotonic_us};
use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_queue::ArrayQueue;
use std::sync::{Arc, atomic::Ordering};

pub const RATE: u32 = 48_000;
pub const FRAME: usize = 960; // 20 ms mono
pub const CAPACITY: usize = FRAME * 5; // bounded to 100 ms at 48 kHz
/// Optional bandwidth/latency tradeoffs. Standard preserves the original behavior.
#[derive(Clone, Copy, Debug, Default, ValueEnum, PartialEq)]
pub enum Profile {
    #[default]
    Standard,
    Efficient,
    Minimum,
}
impl Profile {
    pub fn packet_ms(self) -> u64 {
        match self {
            Self::Standard => 20,
            Self::Efficient => 40,
            Self::Minimum => 60,
        }
    }
    pub fn bitrate(self) -> u32 {
        match self {
            Self::Minimum => 16_000,
            _ => 24_000,
        }
    }
    pub fn dtx(self) -> bool {
        self != Self::Standard
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Original · 24k · 20 ms",
            Self::Efficient => "Efficient · 24k · 40 ms",
            Self::Minimum => "Minimum · 16k · 60 ms",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq)]
pub enum Source {
    Tone,
    Mic,
    /// Deterministic silence for bandwidth checks without opening a microphone.
    Silence,
}
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq)]
pub enum Sink {
    Null,
    Speaker,
}

pub fn devices() -> Result<()> {
    let host = cpal::default_host();
    for (index, device) in host.devices()?.enumerate() {
        println!(
            "[{index}] {}",
            device.name().unwrap_or_else(|_| "unknown device".into())
        );
        if let Ok(c) = device.default_input_config() {
            println!(
                "  input: {} Hz, {} channels, {:?}",
                c.sample_rate().0,
                c.channels(),
                c.sample_format()
            );
        }
        if let Ok(c) = device.default_output_config() {
            println!(
                "  output: {} Hz, {} channels, {:?}",
                c.sample_rate().0,
                c.channels(),
                c.sample_format()
            );
        }
    }
    Ok(())
}
fn device(input: bool, requested: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(name) = requested {
        return host
            .devices()?
            .find(|d| {
                d.name().is_ok_and(|n| n == name)
                    && if input {
                        d.default_input_config().is_ok()
                    } else {
                        d.default_output_config().is_ok()
                    }
            })
            .with_context(|| format!("device {name:?} not found; run `rcof devices`"));
    }
    if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
    .context("no default audio device; use `rcof devices` or synthetic --source tone --sink null")
}

/// Small streaming linear rate converter. This is deliberately a baseline,
/// not a high-quality antialiasing resampler. State survives device callbacks.
pub struct Resampler {
    step: f64,
    next: f64,
    index: u64,
    previous: f32,
}
impl Resampler {
    pub fn new(input: u32, output: u32) -> Self {
        Self {
            step: input as f64 / output as f64,
            next: 0.0,
            index: 0,
            previous: 0.0,
        }
    }
    pub fn push(&mut self, current: f32, mut output: impl FnMut(f32)) {
        let index = self.index as f64;
        while self.next <= index {
            let fraction = (self.next - (index - 1.0)).clamp(0.0, 1.0) as f32;
            output(self.previous + (current - self.previous) * fraction);
            self.next += self.step;
        }
        self.previous = current;
        self.index += 1;
    }
}

#[derive(Clone, Copy)]
pub struct PlaybackSample {
    pub value: f32,
    pub play_at_us: u64,
}

pub struct Audio {
    // Keeping these owned here keeps devices alive; dropping Audio stops capture/playback.
    _input: Option<cpal::Stream>,
    _output: Option<cpal::Stream>,
    pub capture: Arc<ArrayQueue<f32>>,
    pub playback: Arc<ArrayQueue<PlaybackSample>>,
    pub output_rate: u32,
}
impl Audio {
    pub fn open(
        source: Source,
        sink: Sink,
        input: Option<&str>,
        output: Option<&str>,
        metrics: Arc<Metrics>,
        synchronized_video: bool,
    ) -> Result<Self> {
        let capture = Arc::new(ArrayQueue::new(CAPACITY));
        let input_stream = if source == Source::Mic {
            let d = device(true, input)?;
            let c = d
                .default_input_config()
                .context("input device has no supported default configuration")?;
            println!(
                "microphone: {} / {} Hz / {} channels",
                d.name()?,
                c.sample_rate().0,
                c.channels()
            );
            let stream = match c.sample_format() {
                cpal::SampleFormat::F32 => input_stream::<f32>(&d,&c.into(),capture.clone(),metrics.clone()),
                cpal::SampleFormat::I16 => input_stream::<i16>(&d,&c.into(),capture.clone(),metrics.clone()),
                cpal::SampleFormat::U16 => input_stream::<u16>(&d,&c.into(),capture.clone(),metrics.clone()),
                format => bail!("unsupported input sample format {format:?}; select another input device"),
            }.context("cannot open microphone; check macOS microphone permission for this terminal/application")?;
            stream
                .play()
                .context("cannot start microphone; check macOS microphone permission")?;
            Some(stream)
        } else {
            None
        };
        let mut output_rate = RATE;
        let mut playback = Arc::new(ArrayQueue::new(CAPACITY));
        let output_stream = if sink == Sink::Speaker {
            let d = device(false, output)?;
            let c = d
                .default_output_config()
                .context("output device has no supported default configuration")?;
            output_rate = c.sample_rate().0;
            // Leave headroom above the 120 ms A/V deadline for callback batching.
            let capacity_ms = if synchronized_video { 240 } else { 200 };
            playback = Arc::new(ArrayQueue::new((output_rate * capacity_ms / 1000) as usize));
            println!(
                "speaker: {} / {} Hz / {} channels (use headphones)",
                d.name()?,
                output_rate,
                c.channels()
            );
            let stream = match c.sample_format() {
                cpal::SampleFormat::F32 => {
                    output_stream::<f32>(&d, &c.into(), playback.clone(), metrics.clone())
                }
                cpal::SampleFormat::I16 => {
                    output_stream::<i16>(&d, &c.into(), playback.clone(), metrics.clone())
                }
                cpal::SampleFormat::U16 => {
                    output_stream::<u16>(&d, &c.into(), playback.clone(), metrics.clone())
                }
                format => bail!(
                    "unsupported output sample format {format:?}; select another output device"
                ),
            }
            .context("cannot open speaker")?;
            stream.play().context("cannot start speaker")?;
            Some(stream)
        } else {
            None
        };
        Ok(Self {
            _input: input_stream,
            _output: output_stream,
            capture,
            playback,
            output_rate,
        })
    }
}
fn input_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<ArrayQueue<f32>>,
    metrics: Arc<Metrics>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels as usize;
    let mut resampler = Resampler::new(config.sample_rate.0, RATE);
    let errors = metrics.clone();
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            for frame in data.chunks_exact(channels) {
                let mono = frame
                    .iter()
                    .map(|v| <f32 as cpal::FromSample<T>>::from_sample_(*v))
                    .sum::<f32>()
                    / channels as f32;
                resampler.push(mono, |sample| {
                    add(&metrics.captured_samples, 1);
                    if queue.force_push(sample).is_some() {
                        add(&metrics.capture_dropped, 1);
                    }
                });
            }
            metrics
                .capture_peak
                .fetch_max(queue.len() as u64, Ordering::Relaxed);
        },
        move |_| {
            add(&errors.device_errors, 1);
        },
        None,
    )?)
}
/// Apply the presentation deadline once per uninterrupted PCM stream.
/// The hardware sample clock, rather than packet-arrival jitter, paces the rest.
#[derive(Default)]
struct PlaybackClock {
    started: bool,
}
impl PlaybackClock {
    fn ready(&mut self, presentation_us: u64, hardware_us: u64) -> bool {
        if !self.started && presentation_us > hardware_us {
            return false;
        }
        self.started = true;
        true
    }
}

fn output_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<ArrayQueue<PlaybackSample>>,
    metrics: Arc<Metrics>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels as usize;
    let prebuffer = (config.sample_rate.0 / 25) as usize; // 40 ms before playback/rebuffer
    let mut ready = false;
    let mut ever_started = false;
    let mut clock = PlaybackClock::default();
    let mut pending: Option<PlaybackSample> = None;
    let sample_rate = config.sample_rate.0 as u64;
    let errors = metrics.clone();
    Ok(device.build_output_stream(
        config,
        move |data: &mut [T], info| {
            let stamp = info.timestamp();
            let hardware_offset = stamp
                .playback
                .duration_since(&stamp.callback)
                .unwrap_or_default()
                .as_micros() as u64;
            metrics
                .playback_callback_peak
                .fetch_max((data.len() / channels) as u64, Ordering::Relaxed);
            metrics
                .playback_lead_peak
                .fetch_max(hardware_offset, Ordering::Relaxed);
            let callback_time = monotonic_us() + hardware_offset;
            // Some outputs request more than 40 ms in a single callback. Starting
            // with less than that would guarantee an underrun in this callback.
            let packet_samples = (crate::metrics::get(&metrics.rx_packet_samples) * sample_rate
                / RATE as u64) as usize;
            // A long packet needs enough reserve to survive until the next one.
            // Use the received duration, since the peer may use another profile.
            let required = prebuffer
                .max(packet_samples.max(data.len() / channels) + sample_rate as usize / 50);
            if !ready && queue.len() >= required.min(queue.capacity()) {
                ready = true;
                ever_started = true;
            }
            for (index, frame) in data.chunks_exact_mut(channels).enumerate() {
                let sample = if ready {
                    match pending.take().or_else(|| queue.pop()) {
                        Some(v) => {
                            let due = callback_time + index as u64 * 1_000_000 / sample_rate;
                            if !clock.ready(v.play_at_us, due) {
                                pending = Some(v);
                                0.0
                            } else {
                                // Once started, the device clock paces contiguous PCM.
                                // Re-gating every sample turns timer jitter into silence.
                                if v.play_at_us > 0 && due.saturating_sub(v.play_at_us) > 20_000 {
                                    add(&metrics.audio_late_samples, 1);
                                }
                                v.value
                            }
                        }
                        None => {
                            ready = false;
                            clock = PlaybackClock::default();
                            if ever_started {
                                add(&metrics.playback_missing, 1);
                            }
                            0.0
                        }
                    }
                } else {
                    if ever_started {
                        add(&metrics.playback_missing, 1);
                    }
                    0.0
                };
                frame.fill(T::from_sample_(sample));
            }
        },
        move |_| {
            add(&errors.device_errors, 1);
        },
        None,
    )?)
}

pub fn tone(frame: &mut [f32], phase: &mut f32, frequency: f32) {
    let step = std::f32::consts::TAU * frequency / RATE as f32;
    for sample in frame {
        *sample = phase.sin() * 0.08;
        *phase = (*phase + step) % std::f32::consts::TAU;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn playback_does_not_insert_silence_between_jittered_packets() {
        let mut clock = PlaybackClock::default();
        assert!(!clock.ready(80_000, 79_000));
        // Three contiguous 20 ms packets; the middle sender tick was 3 ms late.
        let mut played = 0;
        for (frame, jitter) in [0u64, 3_000, 0].into_iter().enumerate() {
            for sample in 0..FRAME {
                let due = 80_000 + (frame * FRAME + sample) as u64 * 1_000_000 / RATE as u64;
                if clock.ready(due + jitter, due) {
                    played += 1;
                }
            }
        }
        assert_eq!(played, FRAME * 3);
        // An underrun starts a new stream and must honor the deadline again.
        clock = PlaybackClock::default();
        assert!(!clock.ready(200_000, 190_000));
        assert!(clock.ready(200_000, 200_000));
    }
    #[test]
    fn rate_conversion_preserves_duration_and_constant_signal() {
        for (input, output) in [(44100, 48000), (48000, 44100), (48000, 48000)] {
            let mut r = Resampler::new(input, output);
            let mut samples = Vec::new();
            for _ in 0..input {
                r.push(0.25, |v| samples.push(v));
            }
            assert!((samples.len() as i64 - output as i64).abs() <= 2);
            assert!(samples.iter().all(|v| (*v - 0.25).abs() < 0.0001));
        }
    }
    #[test]
    fn all_profiles_decode_silence_resume_and_conceal_correct_duration() {
        for profile in [Profile::Standard, Profile::Efficient, Profile::Minimum] {
            let samples = (RATE as u64 * profile.packet_ms() / 1000) as usize;
            let mut enc =
                opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
            enc.set_bitrate(opus::Bitrate::Bits(profile.bitrate() as i32))
                .unwrap();
            enc.set_dtx(profile.dtx()).unwrap();
            let mut dec = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
            let mut pcm = vec![0.0; samples];
            let mut encoded = [0; 4000];
            let mut decoded = [0.0; 5760];
            let mut phase = 0.0;
            let mut silence_bytes = 0;
            let mut tone_bytes = 0;
            for frame in 0..240 {
                if (40..200).contains(&frame) {
                    pcm.fill(0.0);
                } else {
                    tone(&mut pcm, &mut phase, 440.0);
                }
                let size = enc.encode_float(&pcm, &mut encoded).unwrap();
                assert_eq!(dec.get_nb_samples(&encoded[..size]).unwrap(), samples);
                if (100..140).contains(&frame) {
                    silence_bytes += size;
                }
                if frame >= 200 {
                    tone_bytes += size;
                }
                let n = if frame == 220 {
                    dec.decode_float(&[], &mut decoded[..samples], false)
                        .unwrap()
                } else {
                    dec.decode_float(&encoded[..size], &mut decoded, false)
                        .unwrap()
                };
                assert_eq!(n, samples);
                assert!(decoded[..n].iter().all(|v| v.is_finite()));
                if frame == 239 {
                    assert!(decoded[..n].iter().any(|v| v.abs() > 0.01));
                }
            }
            if profile.dtx() {
                assert!(silence_bytes * 3 < tone_bytes);
            }
        }
    }

    #[test]
    fn opus_roundtrip_retains_signal() {
        let mut enc =
            opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let mut dec = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        let mut raw = [0.0; FRAME];
        tone(&mut raw, &mut 0.0, 440.0);
        let mut compressed = [0; 4000];
        let n = enc.encode_float(&raw, &mut compressed).unwrap();
        let mut pcm = [0.0; FRAME];
        assert_eq!(
            dec.decode_float(&compressed[..n], &mut pcm, false).unwrap(),
            FRAME
        );
        assert!(pcm.iter().any(|v| v.abs() > 0.01));
        assert!(n < FRAME * 4);
    }
}
