//! Native egui interface. Media runs on a separate thread; widgets only send commands.
use crate::{
    audio::{Sink, Source},
    call,
    controls::CallView,
    video::{self, Quality, VideoSource},
};
use anyhow::Result;
use eframe::egui::{self, Color32, RichText, Vec2};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[derive(Clone, Debug, clap::Args)]
pub struct Options {
    #[arg(long, default_value = "a")]
    pub id: String,
    #[arg(long, default_value = "demo")]
    pub room: String,
    #[arg(long, default_value = "127.0.0.1:8790")]
    pub server: SocketAddr,
    #[arg(long)]
    pub auto_join: bool,
    #[arg(long, default_value_t = 0)]
    pub duration: u64,
    #[arg(long)]
    pub stats_file: Option<PathBuf>,
    #[arg(long)]
    pub screenshot: Option<PathBuf>,
    #[arg(long, default_value_t = 8)]
    pub screenshot_after: u64,
    #[arg(long, default_value_t = 0)]
    pub exit_after: u64,
    #[arg(long, default_value_t = 80.0)]
    pub window_x: f32,
    #[arg(long, default_value = "ffmpeg")]
    pub ffmpeg: String,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=5))]
    pub keyframe_seconds: u32,
    #[arg(long, requires = "peer_relay")]
    pub media_port: Option<u16>,
    #[arg(long, requires = "media_port")]
    pub peer_relay: Option<u16>,
    #[arg(long, value_enum, default_value = "standard")]
    pub quality: Quality,
    #[arg(long, value_enum, default_value = "standard")]
    pub audio_profile: crate::audio::Profile,
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u8).range(0..=10))]
    pub audio_complexity: u8,
    #[arg(long)]
    pub no_audio_dtx: bool,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            id: "a".into(),
            room: "demo".into(),
            server: "127.0.0.1:8790".parse().unwrap(),
            auto_join: false,
            duration: 0,
            stats_file: None,
            screenshot: None,
            screenshot_after: 8,
            exit_after: 0,
            window_x: 80.0,
            ffmpeg: "ffmpeg".into(),
            keyframe_seconds: 1,
            media_port: None,
            peer_relay: None,
            quality: Quality::Standard,
            audio_profile: crate::audio::Profile::Standard,
            audio_complexity: 10,
            no_audio_dtx: false,
        }
    }
}
const BG: Color32 = Color32::from_rgb(14, 19, 28);
const CARD: Color32 = Color32::from_rgb(23, 31, 43);
const MUTED: Color32 = Color32::from_rgb(149, 164, 185);
const ACCENT: Color32 = Color32::from_rgb(91, 194, 172);
struct App {
    launch: Options,
    started: Instant,
    view: Arc<CallView>,
    worker: Option<JoinHandle<()>>,
    commands: Option<mpsc::Sender<String>>,
    source: Source,
    sink: Sink,
    video_source: VideoSource,
    quality: Quality,
    camera: String,
    input_device: String,
    output_device: String,
    inputs: Vec<String>,
    outputs: Vec<String>,
    muted: bool,
    camera_off: bool,
    adaptive: bool,
    closing: bool,
    screenshot_requested: bool,
    local_texture: Option<egui::TextureHandle>,
    remote_texture: Option<egui::TextureHandle>,
    local_id: u64,
    remote_id: u64,
    error: Option<String>,
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>, launch: Options) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = CARD;
        style.visuals.selection.bg_fill = Color32::from_rgb(42, 101, 92);
        style.visuals.selection.stroke.color = Color32::WHITE;
        style.spacing.item_spacing = Vec2::new(12.0, 12.0);
        style.spacing.button_padding = Vec2::new(16.0, 10.0);
        cc.egui_ctx.set_style(style);
        let (inputs, outputs) = device_names();
        let view = Arc::new(CallView::default());
        view.state("Ready");
        let quality = launch.quality;
        Self {
            launch,
            started: Instant::now(),
            view,
            worker: None,
            commands: None,
            source: Source::Tone,
            sink: Sink::Null,
            video_source: VideoSource::Pattern,
            quality,
            camera: "0".into(),
            input_device: String::new(),
            output_device: String::new(),
            inputs,
            outputs,
            muted: false,
            camera_off: false,
            adaptive: true,
            closing: false,
            screenshot_requested: false,
            local_texture: None,
            remote_texture: None,
            local_id: 0,
            remote_id: 0,
            error: None,
        }
    }
    fn join(&mut self) {
        if self.worker.is_some() {
            return;
        }
        self.view = Arc::new(CallView::default());
        self.view.state("Joining");
        self.error = None;
        self.muted = false;
        self.camera_off = false;
        let (tx, rx) = mpsc::channel(8);
        self.commands = Some(tx);
        let options = call::Options {
            id: self.launch.id.clone(),
            media_port: self.launch.media_port,
            peer_relay: self.launch.peer_relay,
            adaptive: self.adaptive,
            room: self.launch.room.clone(),
            server: self.launch.server,
            source: self.source,
            sink: self.sink,
            input_device: (!self.input_device.is_empty()).then(|| self.input_device.clone()),
            output_device: (!self.output_device.is_empty()).then(|| self.output_device.clone()),
            tone_hz: if self.launch.id == "a" { 440.0 } else { 660.0 },
            bitrate: None,
            audio_profile: self.launch.audio_profile,
            audio_complexity: self.launch.audio_complexity,
            no_audio_dtx: self.launch.no_audio_dtx,
            duration: self.launch.duration,
            connect_timeout: 120,
            stats_file: self.launch.stats_file.clone(),
            no_stdin: true,
            video: video::Options {
                video: self.video_source,
                quality: self.quality,
                camera: self.camera.clone(),
                ffmpeg: self.launch.ffmpeg.clone(),
                keyframe_seconds: self.launch.keyframe_seconds,
            },
        };
        let view = self.view.clone();
        self.worker = Some(std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|rt| rt.block_on(call::run_controlled(options, rx, view.clone())));
            if let Err(error) = result {
                eprintln!("GUI_ERROR media session: {error:#}");
                view.state(format!("Error: {error:#}"));
            }
            view.video.clear();
            view.ended.store(true, Ordering::Relaxed);
        }));
    }
    fn command(&mut self, command: &str) {
        if let Some(tx) = &self.commands
            && let Err(error) = tx.try_send(command.into())
        {
            self.error = Some(format!("Command was not sent: {error}"));
        }
    }
    fn finish_worker(&mut self) {
        if self.worker.as_ref().is_some_and(|w| w.is_finished()) {
            if self.worker.take().unwrap().join().is_err() {
                eprintln!("GUI_ERROR id={} media worker panicked", self.launch.id);
                self.error = Some("Media worker stopped unexpectedly".into());
            }
            self.commands = None;
        }
    }
    fn refresh_video(&mut self, ctx: &egui::Context) {
        for (frame, texture, id, name) in [
            (
                self.view.video.local.lock().unwrap().clone(),
                &mut self.local_texture,
                &mut self.local_id,
                "local",
            ),
            (
                self.view.video.remote.lock().unwrap().clone(),
                &mut self.remote_texture,
                &mut self.remote_id,
                "remote",
            ),
        ] {
            if let Some(frame) = frame {
                if frame.created_us != *id {
                    let image = egui::ColorImage::from_rgb(
                        [frame.width as usize, frame.height as usize],
                        &frame.rgb,
                    );
                    if let Some(texture) = texture {
                        texture.set(image, egui::TextureOptions::LINEAR);
                    } else {
                        *texture =
                            Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR));
                    }
                    *id = frame.created_us;
                }
            } else {
                *texture = None;
                *id = 0;
            }
        }
    }
    fn video_card(
        ui: &mut egui::Ui,
        title: &str,
        subtitle: &str,
        texture: Option<&egui::TextureHandle>,
        live: bool,
    ) {
        egui::Frame::new()
            .fill(CARD)
            .corner_radius(14)
            .inner_margin(14)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(title).strong().size(16.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(if live { "LIVE" } else { "WAITING" })
                                .color(if live { ACCENT } else { MUTED })
                                .size(11.0),
                        );
                    });
                });
                let size = Vec2::new(ui.available_width(), ui.available_width() * 9.0 / 16.0);
                if live && let Some(texture) = texture {
                    ui.add(egui::Image::new((texture.id(), size)).corner_radius(8));
                } else {
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 8, Color32::from_rgb(10, 15, 23));
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Video will appear here",
                        egui::FontId::proportional(16.0),
                        MUTED,
                    );
                }
                ui.label(RichText::new(subtitle).color(MUTED).size(12.0));
            });
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.finish_worker();
        if self.launch.auto_join {
            self.launch.auto_join = false;
            self.join();
        }
        if ctx.input(|i| i.viewport().close_requested())
            || (self.launch.exit_after > 0
                && self.started.elapsed().as_secs() >= self.launch.exit_after)
        {
            if !self.closing {
                let reason = if ctx.input(|i| i.viewport().close_requested()) {
                    "window close requested"
                } else {
                    "exit-after timer"
                };
                eprintln!(
                    "GUI_EXIT id={} reason={} elapsed_s={:.1}",
                    self.launch.id,
                    reason,
                    self.started.elapsed().as_secs_f64()
                );
            }
            self.closing = true;
            if self.worker.is_some() {
                self.command("quit");
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if self.closing && self.worker.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.refresh_video(ctx);
        let active = self.worker.is_some();
        let state = self.view.state.lock().unwrap().clone();
        let stats = self.view.summary.lock().unwrap().clone();
        egui::TopBottomPanel::top("header")
            .frame(egui::Frame::new().fill(BG).inner_margin(22))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("RCOF")
                            .size(27.0)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("LOCAL CONFERENCE LAB")
                            .size(11.0)
                            .color(MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(state.clone()).color(if state == "connected" {
                            ACCENT
                        } else {
                            MUTED
                        }));
                    });
                });
            });
        egui::SidePanel::left("settings")
            .exact_width(230.0)
            .frame(egui::Frame::new().fill(BG).inner_margin(20))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.heading("Your station");
                    ui.label(
                        RichText::new("Two windows. One private call.")
                            .color(MUTED)
                            .size(12.0),
                    );
                    ui.add_space(8.0);
                    ui.add_enabled_ui(!active, |ui| {
                        ui.label("Instance name");
                        ui.text_edit_singleline(&mut self.launch.id);
                        ui.label("Room");
                        ui.text_edit_singleline(&mut self.launch.room);
                        ui.separator();
                        ui.checkbox(&mut self.adaptive, "Adapt video to loss");
                        ui.label("Audio bandwidth");
                        egui::ComboBox::from_id_salt("audio-profile")
                            .selected_text(self.launch.audio_profile.label())
                            .show_ui(ui, |ui| {
                                for profile in [crate::audio::Profile::Standard, crate::audio::Profile::Efficient, crate::audio::Profile::Minimum] {
                                    ui.selectable_value(&mut self.launch.audio_profile, profile, profile.label());
                                }
                            });
                        ui.collapsing("Audio tuning", |ui| {
                        ui.small("Efficient / Minimum add 20 / 40 ms packetization delay. Quiet-audio compression is optional.");
                        if self.launch.audio_profile != crate::audio::Profile::Standard {
                            ui.checkbox(&mut self.launch.no_audio_dtx, "Keep quiet audio (disable DTX)");
                        }
                        ui.label("Audio encoding effort");
                        ui.add(egui::Slider::new(&mut self.launch.audio_complexity, 0..=10));
                        ui.small("Lower uses less CPU; higher may improve sound. Original: 10.");
                        });
                        ui.label("Audio source");
                        egui::ComboBox::from_id_salt("source")
                            .selected_text(if self.source == Source::Tone {
                                "Test tone"
                            } else {
                                "Microphone"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.source, Source::Tone, "Test tone");
                                ui.selectable_value(&mut self.source, Source::Mic, "Microphone");
                            });
                        if self.source == Source::Mic {
                            device_picker(ui, "input", &mut self.input_device, &self.inputs);
                        }
                        ui.label("Audio output");
                        egui::ComboBox::from_id_salt("sink")
                            .selected_text(if self.sink == Sink::Null {
                                "Silent test"
                            } else {
                                "Headphones / speaker"
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.sink, Sink::Null, "Silent test");
                                ui.selectable_value(
                                    &mut self.sink,
                                    Sink::Speaker,
                                    "Headphones / speaker",
                                );
                            });
                        if self.sink == Sink::Speaker {
                            device_picker(ui, "output", &mut self.output_device, &self.outputs);
                        }
                        ui.label("Video source");
                        egui::ComboBox::from_id_salt("video")
                            .selected_text(match self.video_source {
                                VideoSource::Off => "No outgoing video",
                                VideoSource::Pattern => "Test pattern",
                                VideoSource::Camera => "Camera",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.video_source,
                                    VideoSource::Pattern,
                                    "Test pattern",
                                );
                                ui.selectable_value(
                                    &mut self.video_source,
                                    VideoSource::Camera,
                                    "Camera",
                                );
                                ui.selectable_value(
                                    &mut self.video_source,
                                    VideoSource::Off,
                                    "No outgoing video",
                                );
                            });
                        if self.video_source == VideoSource::Camera {
                            ui.label("Camera index");
                            ui.text_edit_singleline(&mut self.camera);
                        }
                        ui.collapsing("Video recovery tuning", |ui| {
                        ui.label("Video refresh interval (seconds)");
                        ui.add(egui::DragValue::new(&mut self.launch.keyframe_seconds).range(1..=5));
                        ui.small("Longer saves bandwidth; picture recovers more slowly after packet loss.");
                        });
                        ui.label("Video quality");
                        egui::ComboBox::from_id_salt("quality")
                            .selected_text(self.quality.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.quality,
                                    Quality::Tiny,
                                    Quality::Tiny.label(),
                                );
                                ui.selectable_value(
                                    &mut self.quality,
                                    Quality::Minimal,
                                    Quality::Minimal.label(),
                                );
                                ui.selectable_value(
                                    &mut self.quality,
                                    Quality::Low,
                                    Quality::Low.label(),
                                );
                                ui.selectable_value(
                                    &mut self.quality,
                                    Quality::Standard,
                                    Quality::Standard.label(),
                                );
                            });
                    });
                    ui.add_space(10.0);
                    if !active
                        && ui
                            .add_sized(
                                [190.0, 42.0],
                                egui::Button::new(RichText::new("Join call").strong().color(BG))
                                    .fill(ACCENT),
                            )
                            .clicked()
                    {
                        self.join();
                    }
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Local connection only")
                            .color(ACCENT)
                            .size(12.0),
                    );
                    ui.label(
                        RichText::new(self.launch.server.to_string())
                            .color(MUTED)
                            .monospace()
                            .size(11.0),
                    );
                    if self.launch.peer_relay.is_some() {
                        ui.colored_label(ACCENT, "Simulated network active");
                    }
                    if self.source == Source::Mic && self.sink == Sink::Speaker {
                        ui.colored_label(
                            Color32::from_rgb(250, 192, 91),
                            "Use headphones to avoid feedback.",
                        );
                    }
                });
            });
        egui::TopBottomPanel::bottom("controls")
            .frame(egui::Frame::new().fill(CARD).inner_margin(18))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(active, |ui| {
                        if ui
                            .button(if self.muted {
                                "Unmute audio"
                            } else {
                                "Mute audio"
                            })
                            .clicked()
                        {
                            self.muted = !self.muted;
                            self.command(if self.muted { "mute" } else { "unmute" });
                        }
                        if ui
                            .add_enabled(
                                self.video_source != VideoSource::Off,
                                egui::Button::new(if self.camera_off {
                                    "Turn video on"
                                } else {
                                    "Turn video off"
                                }),
                            )
                            .clicked()
                        {
                            self.camera_off = !self.camera_off;
                            self.command(if self.camera_off {
                                "camera-off"
                            } else {
                                "camera-on"
                            });
                        }
                        if ui
                            .add(
                                egui::Button::new("Leave call")
                                    .fill(Color32::from_rgb(134, 48, 64)),
                            )
                            .clicked()
                        {
                            eprintln!("GUI_EVENT id={} leave button", self.launch.id);
                            self.command("quit");
                        }
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new("P2 / NATIVE RUST").color(MUTED).size(11.0));
                    });
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(BG).inner_margin(22)).show(ctx,|ui| {
            ui.heading(format!("Room / {}",self.launch.room));ui.label(RichText::new("Audio + video, running entirely on this Mac.").color(MUTED));ui.add_space(14.0);
            let local_live=self.view.video.local.lock().unwrap().as_ref().is_some_and(|f|f.received_at.elapsed()<Duration::from_secs(1));
            let remote_live=self.view.video.remote.lock().unwrap().as_ref().is_some_and(|f|f.received_at.elapsed()<Duration::from_secs(1));
            ui.columns(2,|cols| {
                Self::video_card(&mut cols[0],"You",&format!("{} · {:?}",self.launch.id,self.video_source),self.local_texture.as_ref(),local_live);
                Self::video_card(&mut cols[1],"Other station","Decoded video from the other instance",self.remote_texture.as_ref(),remote_live);
            });
            ui.add_space(18.0);ui.label(RichText::new("CALL MEASUREMENTS").size(11.0).color(MUTED));
            egui::Frame::new().fill(CARD).corner_radius(12).inner_margin(18).show(ui,|ui| {
                ui.columns(3,|cols| {
                    metric(&mut cols[0],"Audio received",stats.as_ref().map(|s|format!("{} frames",s.rx_frames)).unwrap_or_else(||"—".into()));
                    metric(&mut cols[1],"Video received",stats.as_ref().map(|s|format!("{} frames",s.video_rx_frames)).unwrap_or_else(||"—".into()));
                    metric(&mut cols[2],"Send rate · session avg",stats.as_ref().map(|s|format!("{:.0} kbit/s",s.transport_tx_kbps)).unwrap_or_else(||"—".into()));
                });
            });
            if active && self.video_source != VideoSource::Off {ui.label(RichText::new(format!("Sending: {} · adaptation {}",Quality::from_level(self.view.video.quality.load(Ordering::Relaxed)).label(),if self.adaptive {"on"}else{"off"})).color(ACCENT).size(12.0));}
            ui.add_space(12.0);ui.label(RichText::new("Test sources are silent by default. Choose a microphone and headphones before joining to hear a call.").color(MUTED).size(12.0));
            if state.starts_with("Error:") || state.starts_with("error:") {ui.colored_label(Color32::from_rgb(250,142,150),&state);}
            if let Some(error)=&self.error {ui.colored_label(Color32::from_rgb(250,142,150),error);}
        });
        if let Some(path) = &self.launch.screenshot {
            if !self.screenshot_requested
                && self.started.elapsed().as_secs() >= self.launch.screenshot_after
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                self.screenshot_requested = true;
            }
            for event in ctx.input(|i| i.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                    if let Err(error) = image::save_buffer(
                        path,
                        &bytes,
                        image.width() as u32,
                        image.height() as u32,
                        image::ColorType::Rgba8,
                    ) {
                        self.error = Some(format!("Screenshot: {error}"));
                    }
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}
impl Drop for App {
    fn drop(&mut self) {
        eprintln!(
            "GUI_EXIT id={} app dropped elapsed_s={:.1}",
            self.launch.id,
            self.started.elapsed().as_secs_f64()
        );
        self.command("quit");
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn metric(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(RichText::new(label).size(11.0).color(MUTED));
    ui.label(RichText::new(value).size(21.0).strong());
}
fn device_picker(ui: &mut egui::Ui, id: &str, selected: &mut String, names: &[String]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(if selected.is_empty() {
            "System default"
        } else {
            selected.as_str()
        })
        .width(185.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, String::new(), "System default");
            for name in names {
                ui.selectable_value(selected, name.clone(), name);
            }
        });
}
fn device_names() -> (Vec<String>, Vec<String>) {
    use cpal::traits::{DeviceTrait, HostTrait};
    let mut inputs = vec![];
    let mut outputs = vec![];
    if let Ok(devices) = cpal::default_host().devices() {
        for d in devices {
            if let Ok(name) = d.name() {
                if d.default_input_config().is_ok() {
                    inputs.push(name.clone());
                }
                if d.default_output_config().is_ok() {
                    outputs.push(name);
                }
            }
        }
    }
    (inputs, outputs)
}
pub fn run(options: Options) -> Result<()> {
    let title = format!("RCOF — {}", options.id);
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 720.0])
            .with_min_inner_size([900.0, 660.0])
            .with_position([options.window_x, 90.0]),
        ..Default::default()
    };
    eframe::run_native(
        &title,
        native,
        Box::new(move |cc| Ok(Box::new(App::new(cc, options)))),
    )
    .map_err(|e| anyhow::anyhow!("native window: {e}"))
}
