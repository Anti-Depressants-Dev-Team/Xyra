#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use chrono::Utc;
use eframe::egui::{self, Color32, RichText};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState, hotkey::HotKey};
use uuid::Uuid;
use xyra::{
    audio::{
        AudioDevice, enumerate_audio_devices, is_desktop_audio_device, is_microphone_device,
        recommended_desktop_audio, recommended_microphone,
    },
    capture::CaptureManager,
    config::{
        AppConfig, CaptureQuality, ClipHotkey, EncoderBackend, HotkeyModifier, VideoAspectRatio,
        VideoCodec, managed_ffmpeg_path,
    },
    display::{MonitorInfo, enumerate_monitors, selected_monitor},
    ffmpeg::{FfmpegInstaller, InstallState},
    library::scan_clips,
    model::{Clip, EditProject, Platform, PublishJob, PublishStatus, PublishTarget},
    player::{PREVIEW_HEIGHT, PREVIEW_WIDTH, VideoPlayer},
    publish::{PublishQueue, connection_help},
    startup::sync_startup_registration,
    system_tray::{SystemTray, TrayAction},
};

fn main() -> eframe::Result {
    let autostart_launch = std::env::args().any(|argument| argument == "--autostart");
    let start_hidden = autostart_launch
        && AppConfig::load().is_ok_and(|config| config.start_minimized_on_system_start);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("Xyra")
            .with_visible(!start_hidden),
        ..Default::default()
    };
    eframe::run_native(
        "Xyra",
        options,
        Box::new(move |cc| Ok(Box::new(XyraApp::new(cc, start_hidden)))),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Library,
    Editor,
    Publish,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClipCardAction {
    OpenEditor,
    Play,
    RevealInExplorer,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Editor => "Editor",
            Self::Publish => "Publish",
            Self::Settings => "Settings",
        }
    }
}

struct XyraApp {
    config: AppConfig,
    capture: CaptureManager,
    clips: Vec<Clip>,
    selected: Option<usize>,
    project: Option<EditProject>,
    page: Page,
    targets: Vec<PublishTarget>,
    queue: PublishQueue,
    status: String,
    ffmpeg_ready: bool,
    ffmpeg_input: String,
    ffmpeg_installer: FfmpegInstaller,
    player: VideoPlayer,
    player_texture: Option<egui::TextureHandle>,
    search: String,
    monitors: Vec<MonitorInfo>,
    audio_devices: Vec<AudioDevice>,
    saved_config_state: String,
    config_save_due: Option<Instant>,
    hotkey_manager: Option<GlobalHotKeyManager>,
    registered_hotkeys: Vec<(HotKey, u32)>,
    hotkey_config_state: String,
    hotkey_status: String,
    tray: Option<SystemTray>,
    quit_requested: bool,
}

impl XyraApp {
    fn new(cc: &eframe::CreationContext<'_>, started_hidden: bool) -> Self {
        configure_style(&cc.egui_ctx);
        let mut config = AppConfig::load().unwrap_or_default();
        let ffmpeg_ready = CaptureManager::ffmpeg_available(&config);
        let audio_devices = if ffmpeg_ready {
            enumerate_audio_devices(&config.ffmpeg_path).unwrap_or_default()
        } else {
            Vec::new()
        };
        sync_audio_device_selection(&mut config, &audio_devices);
        // Rewrite older, partial config files immediately so every newly added setting persists.
        let _ = config.save();
        let saved_config_state = serde_json::to_string(&config).unwrap_or_default();
        let ffmpeg_input = config.ffmpeg_path.display().to_string();
        let ffmpeg_installer = FfmpegInstaller::new(
            config.ffmpeg_path.clone(),
            config.ffmpeg_path == managed_ffmpeg_path(),
        );
        let mut status = if ffmpeg_ready {
            "Ready to capture".into()
        } else {
            ffmpeg_installer.state().label()
        };
        if let Err(error) = sync_startup_registration(config.start_with_windows) {
            status = format!("Could not update Windows startup: {error}");
        }
        let tray = match SystemTray::new(&cc.egui_ctx) {
            Ok(tray) => Some(tray),
            Err(error) => {
                status = format!("System tray is unavailable: {error}");
                if started_hidden {
                    cc.egui_ctx
                        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                }
                None
            }
        };
        if started_hidden && tray.is_some() {
            status = "Xyra started in the system tray".into();
        }
        let clips = scan_clips(
            &config.clips_directory,
            ffmpeg_ready.then_some(config.ffmpeg_path.as_path()),
        )
        .unwrap_or_default();
        let mut app = Self {
            config,
            capture: CaptureManager::default(),
            clips,
            selected: None,
            project: None,
            page: Page::Library,
            targets: Platform::ALL.into_iter().map(PublishTarget::new).collect(),
            queue: PublishQueue::default(),
            status,
            ffmpeg_ready,
            ffmpeg_input,
            ffmpeg_installer,
            player: VideoPlayer::default(),
            player_texture: None,
            search: String::new(),
            monitors: enumerate_monitors(),
            audio_devices,
            saved_config_state,
            config_save_due: None,
            hotkey_manager: GlobalHotKeyManager::new().ok(),
            registered_hotkeys: Vec::new(),
            hotkey_config_state: String::new(),
            hotkey_status: String::new(),
            tray,
            quit_requested: false,
        };
        app.refresh_hotkeys();
        app
    }

    fn refresh_hotkeys(&mut self) {
        if let Some(manager) = &self.hotkey_manager {
            let old_hotkeys = self
                .registered_hotkeys
                .iter()
                .map(|(hotkey, _)| *hotkey)
                .collect::<Vec<_>>();
            let _ = manager.unregister_all(&old_hotkeys);
        }
        self.registered_hotkeys.clear();
        self.hotkey_config_state =
            serde_json::to_string(&self.config.clip_hotkeys).unwrap_or_default();

        let Some(manager) = &self.hotkey_manager else {
            self.hotkey_status = "Global hotkeys are unavailable on this system".into();
            return;
        };

        let mut failures = Vec::new();
        for configured in self
            .config
            .clip_hotkeys
            .iter()
            .filter(|hotkey| hotkey.enabled)
        {
            let hotkey = match native_hotkey(configured) {
                Ok(hotkey) => hotkey,
                Err(error) => {
                    failures.push(error);
                    continue;
                }
            };
            if self
                .registered_hotkeys
                .iter()
                .any(|(registered, _)| registered.id() == hotkey.id())
            {
                failures.push(format!("{} is assigned more than once", configured.label()));
                continue;
            }
            match manager.register(hotkey) {
                Ok(()) => self
                    .registered_hotkeys
                    .push((hotkey, configured.clip_seconds.max(5))),
                Err(error) => failures.push(format!(
                    "{} could not be registered: {error}",
                    configured.label()
                )),
            }
        }

        self.hotkey_status = if failures.is_empty() {
            format!(
                "{} global clip hotkey{} active",
                self.registered_hotkeys.len(),
                if self.registered_hotkeys.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )
        } else {
            failures.join("  ")
        };
    }

    fn poll_hotkeys(&mut self) {
        let current_state = serde_json::to_string(&self.config.clip_hotkeys).unwrap_or_default();
        if current_state != self.hotkey_config_state {
            self.refresh_hotkeys();
        }

        let pressed = GlobalHotKeyEvent::receiver()
            .try_iter()
            .filter(|event| event.state == HotKeyState::Pressed)
            .filter_map(|event| {
                self.registered_hotkeys
                    .iter()
                    .find(|(hotkey, _)| hotkey.id() == event.id)
                    .map(|(_, seconds)| *seconds)
            })
            .collect::<Vec<_>>();
        for seconds in pressed {
            self.save_clip_duration(seconds, false);
        }
    }

    fn poll_config_auto_save(&mut self, ctx: &egui::Context) {
        let Ok(current_state) = serde_json::to_string(&self.config) else {
            return;
        };
        if current_state == self.saved_config_state {
            self.config_save_due = None;
            return;
        }

        let now = Instant::now();
        let due = *self
            .config_save_due
            .get_or_insert_with(|| now + Duration::from_millis(350));
        if now < due {
            ctx.request_repaint_after(due - now);
            return;
        }

        match self.config.save() {
            Ok(()) => {
                self.saved_config_state = current_state;
                self.config_save_due = None;
                if let Err(error) = sync_startup_registration(self.config.start_with_windows) {
                    self.status = format!("Settings saved, but Windows startup failed: {error}");
                }
            }
            Err(error) => {
                self.status = format!("Could not auto-save settings: {error}");
                self.config_save_due = Some(now + Duration::from_secs(2));
            }
        }
    }

    fn refresh_audio_devices(&mut self) {
        match enumerate_audio_devices(&self.config.ffmpeg_path) {
            Ok(devices) => {
                self.audio_devices = devices;
                sync_audio_device_selection(&mut self.config, &self.audio_devices);
                self.status = format!(
                    "Detected {} Windows audio endpoint{}",
                    self.audio_devices.len(),
                    if self.audio_devices.len() == 1 {
                        ""
                    } else {
                        "s"
                    }
                );
            }
            Err(error) => self.status = format!("Could not scan audio devices: {error}"),
        }
    }

    fn selected_clip(&self) -> Option<&Clip> {
        self.selected.and_then(|index| self.clips.get(index))
    }

    fn start_or_stop(&mut self) {
        if self.capture.is_running() {
            match self.capture.stop() {
                Ok(()) => self.status = "Replay buffer stopped".into(),
                Err(error) => self.status = error.to_string(),
            }
        } else {
            match self.capture.start(&self.config) {
                Ok(()) => self.status = "Replay buffer is recording".into(),
                Err(error) => self.status = error.to_string(),
            }
        }
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        let actions = self
            .tray
            .as_ref()
            .map(SystemTray::drain_actions)
            .unwrap_or_default();
        for action in actions {
            match action {
                TrayAction::Show => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayAction::ToggleReplayBuffer => self.start_or_stop(),
                TrayAction::SaveClip => self.save_clip_duration(self.config.clip_seconds, false),
                TrayAction::Quit => {
                    self.quit_requested = true;
                    let _ = self.config.save();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

        let running = self.capture.is_running();
        if let Some(tray) = self.tray.as_mut() {
            tray.update(running, self.config.clip_seconds);
        }
    }

    fn handle_window_close(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested
            && self.config.minimize_to_tray
            && self.tray.is_some()
            && !self.quit_requested
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.status = "Xyra is still running in the system tray".into();
        }
    }

    fn save_clip(&mut self) {
        self.save_clip_duration(self.config.clip_seconds, true);
    }

    fn save_clip_duration(&mut self, seconds: u32, open_after_save: bool) {
        match self.capture.save_replay_duration(&self.config, seconds) {
            Ok(clip) => {
                self.status = format!(
                    "Saved the previous {} sec to {}",
                    clip.duration_secs.round() as u32,
                    clip.path.display()
                );
                self.clips.insert(0, clip);
                if open_after_save {
                    self.select_clip(0);
                }
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn select_clip(&mut self, index: usize) {
        self.selected = Some(index);
        if let Some(clip) = self.clips.get(index) {
            self.project = Some(EditProject::for_clip(clip));
            self.player.load(clip.path.clone(), clip.duration_secs);
            self.player_texture = None;
            if self.ffmpeg_ready {
                self.player.request_preview(&self.config.ffmpeg_path);
            }
        }
    }

    fn queue_publish(&mut self) {
        let Some(clip) = self.selected_clip().cloned() else {
            self.status = "Select a clip before publishing".into();
            return;
        };
        let job = PublishJob {
            id: Uuid::new_v4(),
            clip_id: clip.id,
            title: clip.title,
            description: clip.description,
            targets: self.targets.clone(),
            created_at: Utc::now(),
            status: PublishStatus::Queued,
        };
        match self.queue.enqueue(job) {
            Ok(()) => {
                self.status = "Upload queued (account connection is the next milestone)".into()
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    fn poll_ffmpeg_install(&mut self) {
        if !self.ffmpeg_installer.poll() {
            return;
        }
        match self.ffmpeg_installer.state().clone() {
            InstallState::Ready => {
                self.config.ffmpeg_path = self.ffmpeg_installer.destination().to_path_buf();
                self.ffmpeg_input = self.config.ffmpeg_path.display().to_string();
                self.ffmpeg_ready = CaptureManager::ffmpeg_available(&self.config);
                self.refresh_audio_devices();
                if let Ok(clips) =
                    scan_clips(&self.config.clips_directory, Some(&self.config.ffmpeg_path))
                {
                    self.clips = clips;
                }
                self.status = "FFmpeg installed automatically. Capture is ready.".into();
                let _ = self.config.save();
            }
            InstallState::Failed(error) => {
                self.ffmpeg_ready = false;
                self.status = format!("FFmpeg setup failed: {error}");
            }
            state => self.status = state.label(),
        }
    }

    fn use_managed_ffmpeg(&mut self) {
        let path = managed_ffmpeg_path();
        self.config.ffmpeg_path = path.clone();
        self.ffmpeg_input = path.display().to_string();
        self.ffmpeg_ready = CaptureManager::ffmpeg_available(&self.config);
        self.ffmpeg_installer = FfmpegInstaller::new(path, true);
        self.status = self.ffmpeg_installer.state().label();
        let _ = self.config.save();
    }

    fn poll_player(&mut self, ctx: &egui::Context) {
        if let Some(frame) = self.player.poll() {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [PREVIEW_WIDTH, PREVIEW_HEIGHT],
                &frame.rgba,
            );
            if let Some(texture) = self.player_texture.as_mut() {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.player_texture = Some(ctx.load_texture(
                    "xyra-video-preview",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
    }

    fn player_panel(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(Color32::BLACK)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(12)
            .show(ui, |ui| {
                let width = ui.available_width();
                let video_size = egui::vec2(width, width * 9.0 / 16.0);
                let (video_rect, _) = ui.allocate_exact_size(video_size, egui::Sense::hover());
                ui.painter()
                    .rect_filled(video_rect, 9.0, Color32::from_rgb(3, 4, 7));
                if let Some(texture) = &self.player_texture {
                    ui.painter().image(
                        texture.id(),
                        video_rect,
                        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    ui.painter().text(
                        video_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Loading preview...",
                        egui::FontId::proportional(12.0),
                        MUTED,
                    );
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let playing = self.player.is_playing();
                    if ui
                        .add_sized(
                            [74.0, 34.0],
                            egui::Button::new(if playing { "Pause" } else { "Play" })
                                .fill(ACCENT)
                                .corner_radius(8),
                        )
                        .clicked()
                    {
                        if playing {
                            self.player.pause();
                        } else {
                            self.player.play(&self.config.ffmpeg_path);
                        }
                    }

                    let mut position = self.player.position_secs();
                    let duration = self.player.duration_secs().max(0.01);
                    let slider_width = (ui.available_width() - 104.0).max(80.0);
                    let response = ui.add_sized(
                        [slider_width, 24.0],
                        egui::Slider::new(&mut position, 0.0..=duration).show_value(false),
                    );
                    if response.changed() {
                        self.player.seek(&self.config.ffmpeg_path, position);
                    }
                    ui.label(
                        RichText::new(format!(
                            "{} / {}",
                            format_time(position),
                            format_time(self.player.duration_secs())
                        ))
                        .size(10.0)
                        .color(MUTED),
                    );
                });
                if let Some(error) = self.player.error() {
                    ui.label(RichText::new(error).size(10.0).color(DANGER));
                }
            });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, SIDEBAR);

        let inner = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 16.0, rect.top()),
            egui::pos2(rect.right() - 16.0, rect.bottom()),
        );
        let engine_rect = egui::Rect::from_min_max(
            egui::pos2(inner.left(), inner.bottom() - 100.0),
            egui::pos2(inner.right(), inner.bottom() - 20.0),
        );
        let navigation_rect = egui::Rect::from_min_max(
            inner.min,
            egui::pos2(inner.right(), engine_rect.top() - 16.0),
        );
        let mut navigation_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(navigation_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );

        navigation_ui.add_space(24.0);
        navigation_ui.horizontal(|ui| {
            let (mark, _) = ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::hover());
            ui.painter().rect_filled(mark, 10.0, ACCENT);
            ui.painter().line_segment(
                [
                    mark.left_top() + egui::vec2(9.0, 9.0),
                    mark.right_bottom() - egui::vec2(9.0, 9.0),
                ],
                egui::Stroke::new(2.5, Color32::WHITE),
            );
            ui.painter().line_segment(
                [
                    mark.right_top() + egui::vec2(-9.0, 9.0),
                    mark.left_bottom() + egui::vec2(9.0, -9.0),
                ],
                egui::Stroke::new(2.5, Color32::WHITE),
            );
            ui.vertical(|ui| {
                ui.label(RichText::new("XYRA").size(20.0).strong().color(TEXT));
                ui.label(RichText::new("CAPTURE STUDIO").size(9.0).color(MUTED));
            });
        });
        navigation_ui.add_space(38.0);
        navigation_ui.label(RichText::new("WORKSPACE").size(10.0).strong().color(MUTED));
        navigation_ui.add_space(10.0);
        nav_button(&mut navigation_ui, &mut self.page, Page::Library, "Clips");
        nav_button(&mut navigation_ui, &mut self.page, Page::Editor, "Editor");
        nav_button(&mut navigation_ui, &mut self.page, Page::Publish, "Publish");
        navigation_ui.add_space(24.0);
        navigation_ui.label(RichText::new("SYSTEM").size(10.0).strong().color(MUTED));
        navigation_ui.add_space(10.0);
        nav_button(
            &mut navigation_ui,
            &mut self.page,
            Page::Settings,
            "Settings",
        );

        let mut engine_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(engine_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(12)
            .inner_margin(14)
            .show(&mut engine_ui, |ui| {
                ui.set_width((engine_rect.width() - 28.0).max(100.0));
                ui.label(
                    RichText::new("CAPTURE ENGINE")
                        .size(9.0)
                        .strong()
                        .color(MUTED),
                );
                ui.add_space(5.0);
                ui.horizontal(|ui| {
                    let (label, color) =
                        ffmpeg_status(self.ffmpeg_installer.state(), self.ffmpeg_ready);
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, color);
                    ui.label(RichText::new(label).size(12.0).color(TEXT));
                });
            });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.capture.is_running();
        egui::Frame::new()
            .fill(BG)
            .inner_margin(egui::Margin::symmetric(28, 16))
            .show(ui, |ui| {
                ui.set_height(44.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(self.page.title())
                                .size(20.0)
                                .strong()
                                .color(TEXT),
                        );
                        ui.label(
                            RichText::new("Your moments, your way")
                                .size(11.0)
                                .color(MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled_ui(self.ffmpeg_ready || running, |ui| {
                                ui.add_sized(
                                    [144.0, 38.0],
                                    egui::Button::new(
                                        RichText::new(if running {
                                            "Stop capture"
                                        } else {
                                            "Start capture"
                                        })
                                        .strong(),
                                    )
                                    .fill(if running { DANGER } else { ACCENT })
                                    .corner_radius(9),
                                )
                            })
                            .inner
                            .clicked()
                        {
                            self.start_or_stop();
                        }
                        ui.add_enabled_ui(running, |ui| {
                            if ui
                                .add_sized(
                                    [120.0, 38.0],
                                    egui::Button::new(
                                        RichText::new(format!(
                                            "Save {}s clip",
                                            self.config.clip_seconds
                                        ))
                                        .strong(),
                                    )
                                    .fill(SURFACE_2)
                                    .stroke(egui::Stroke::new(1.0, BORDER))
                                    .corner_radius(9),
                                )
                                .clicked()
                            {
                                self.save_clip();
                            }
                        });
                        engine_badge(
                            ui,
                            running,
                            self.ffmpeg_installer.state(),
                            self.ffmpeg_ready,
                        );
                    });
                });
            });
        ui.painter().hline(
            ui.max_rect().x_range(),
            ui.cursor().top(),
            egui::Stroke::new(1.0, BORDER),
        );
    }

    fn library_page(&mut self, ui: &mut egui::Ui) {
        page_heading(
            ui,
            "Your clips",
            "Everything you capture stays private on this device until you publish it.",
        );
        ui.horizontal_top(|ui| {
            let card_width = ((ui.available_width() - 20.0) / 3.0).max(150.0);
            metric_card(
                ui,
                "TOTAL CLIPS",
                &self.clips.len().to_string(),
                "Saved locally",
                ACCENT,
                card_width,
            );
            metric_card(
                ui,
                "REPLAY LENGTH",
                &format!("{} sec", self.config.clip_seconds),
                "Instant capture",
                CYAN,
                card_width,
            );
            metric_card(
                ui,
                "UPLOAD QUEUE",
                &self.queue.len().to_string(),
                "Across platforms",
                SUCCESS,
                card_width,
            );
        });
        ui.add_space(28.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Recent clips")
                    .size(18.0)
                    .strong()
                    .color(TEXT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_sized(
                    [220.0, 34.0],
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search clips...")
                        .margin(egui::Margin::symmetric(12, 8)),
                );
            });
        });
        ui.add_space(12.0);

        let visible: Vec<(usize, Clip)> = self
            .clips
            .iter()
            .cloned()
            .enumerate()
            .filter(|(_, clip)| {
                clip.title
                    .to_lowercase()
                    .contains(&self.search.to_lowercase())
            })
            .collect();
        if visible.is_empty() {
            empty_state(
                ui,
                if self.clips.is_empty() {
                    "Your best moments will show up here"
                } else {
                    "No matching clips"
                },
                if self.clips.is_empty() {
                    "Start the replay buffer, play a game, then save your last moment."
                } else {
                    "Try a different search."
                },
                self.clips.is_empty(),
            );
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            let columns = if ui.available_width() > 850.0 { 3 } else { 2 };
            let mut action = None;
            for row in visible.chunks(columns) {
                ui.columns(columns, |column_uis| {
                    for (column, (index, clip)) in row.iter().enumerate() {
                        if let Some(card_action) =
                            clip_card(&mut column_uis[column], clip, self.selected == Some(*index))
                        {
                            action = Some((*index, card_action));
                        }
                    }
                });
                ui.add_space(12.0);
            }
            if let Some((index, card_action)) = action {
                self.select_clip(index);
                match card_action {
                    ClipCardAction::OpenEditor => {
                        self.page = Page::Editor;
                        self.status = format!("Opened {} in the editor", self.clips[index].title);
                    }
                    ClipCardAction::Play => {
                        if let Err(error) = open::that(&self.clips[index].path) {
                            self.status = format!("Could not play clip: {error}");
                        } else {
                            self.status = format!("Playing {}", self.clips[index].title);
                        }
                    }
                    ClipCardAction::RevealInExplorer => {
                        if let Err(error) = reveal_in_file_manager(&self.clips[index].path) {
                            self.status = format!("Could not open File Explorer: {error}");
                        } else {
                            self.status =
                                format!("Showing {} in File Explorer", self.clips[index].title);
                        }
                    }
                }
            }
        });
    }

    fn editor_page(&mut self, ui: &mut egui::Ui) {
        page_heading(
            ui,
            "Clip editor",
            "Polish the moment, trim the noise, and export a clean MP4.",
        );
        let Some(index) = self.selected else {
            empty_state(
                ui,
                "Choose a clip to start editing",
                "Select a moment from your Library first.",
                false,
            );
            return;
        };
        let clip = self.clips[index].clone();
        if self
            .project
            .as_ref()
            .is_none_or(|project| project.clip_id != clip.id)
        {
            self.project = Some(EditProject::for_clip(&clip));
        }
        ui.columns(2, |columns| {
            self.player_panel(&mut columns[0]);
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(egui::Stroke::new(1.0, BORDER))
                .corner_radius(14)
                .inner_margin(20)
                .show(&mut columns[1], |ui| {
                    ui.heading(RichText::new("Clip details").color(TEXT));
                    ui.add_space(14.0);
                    if let Some(clip) = self.clips.get_mut(index) {
                        field_label(ui, "TITLE");
                        ui.text_edit_singleline(&mut clip.title);
                        ui.add_space(12.0);
                        field_label(ui, "DESCRIPTION");
                        ui.add_sized(
                            [ui.available_width(), 82.0],
                            egui::TextEdit::multiline(&mut clip.description),
                        );
                    }
                    ui.add_space(18.0);
                    if let Some(project) = self.project.as_mut() {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("IN  {:.1}s", project.trim_start_secs))
                                    .strong()
                                    .color(CYAN),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "OUT  {:.1}s",
                                            project.trim_end_secs
                                        ))
                                        .strong()
                                        .color(ACCENT_LIGHT),
                                    );
                                },
                            );
                        });
                        ui.add(
                            egui::Slider::new(
                                &mut project.trim_start_secs,
                                0.0..=project.trim_end_secs,
                            )
                            .show_value(false),
                        );
                        ui.add(
                            egui::Slider::new(
                                &mut project.trim_end_secs,
                                project.trim_start_secs
                                    ..=clip.duration_secs.max(project.trim_start_secs),
                            )
                            .show_value(false),
                        );
                        ui.add_space(18.0);
                        if ui
                            .add_sized(
                                [ui.available_width(), 40.0],
                                egui::Button::new(RichText::new("Export trimmed clip").strong())
                                    .fill(ACCENT)
                                    .corner_radius(9),
                            )
                            .clicked()
                        {
                            let output = export_path(&clip.path);
                            match CaptureManager::export_trimmed(
                                &self.config,
                                &clip.path,
                                &output,
                                project.trim_start_secs,
                                project.trim_end_secs,
                            ) {
                                Ok(()) => self.status = format!("Exported {}", output.display()),
                                Err(error) => self.status = error.to_string(),
                            }
                        }
                    }
                });
        });
    }

    fn publish_page(&mut self, ui: &mut egui::Ui) {
        page_heading(
            ui,
            "Publish your moment",
            "Send one clip to multiple platforms with the right visibility on each.",
        );
        let selection = self
            .selected
            .map(|index| self.clips[index].title.as_str())
            .unwrap_or("Choose a clip from Library");
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(20)
            .show(ui, |ui| {
                field_label(ui, "SELECTED CLIP");
                ui.label(RichText::new(selection).size(17.0).strong().color(
                    if self.selected.is_some() {
                        TEXT
                    } else {
                        WARNING
                    },
                ));
            });
        ui.add_space(16.0);
        for target in &mut self.targets {
            egui::Frame::new()
                .fill(SURFACE)
                .stroke(egui::Stroke::new(1.0, BORDER))
                .corner_radius(14)
                .inner_margin(20)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut target.enabled, "");
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(target.platform.label())
                                    .size(17.0)
                                    .strong()
                                    .color(TEXT),
                            );
                            ui.label(
                                RichText::new(connection_help(target.platform))
                                    .size(11.0)
                                    .color(MUTED),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add_enabled_ui(target.enabled, |ui| {
                                egui::ComboBox::from_id_salt(target.platform.label())
                                    .selected_text(target.visibility.label())
                                    .show_ui(ui, |ui| {
                                        for visibility in target.platform.supported_visibilities() {
                                            ui.selectable_value(
                                                &mut target.visibility,
                                                *visibility,
                                                visibility.label(),
                                            );
                                        }
                                    });
                            });
                        });
                    });
                });
            ui.add_space(10.0);
        }
        if ui
            .add_sized(
                [210.0, 42.0],
                egui::Button::new(RichText::new("Add to upload queue").strong())
                    .fill(ACCENT)
                    .corner_radius(9),
            )
            .clicked()
        {
            self.queue_publish();
        }
        ui.add_space(24.0);
        section_title(ui, &format!("Upload queue ({})", self.queue.len()));
        if self.queue.is_empty() {
            ui.label(RichText::new("Queued uploads will appear here.").color(MUTED));
        }
        for job in self.queue.jobs() {
            ui.label(RichText::new(format!("{}  -  queued", job.title)).color(TEXT));
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        page_heading(
            ui,
            "Settings",
            "Choose exactly what Xyra records and how it encodes each clip.",
        );
        ui.label(
            RichText::new("All changes save automatically.")
                .size(11.0)
                .color(SUCCESS),
        );
        ui.add_space(10.0);
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(24)
            .show(ui, |ui| {
                section_title(ui, "Windows startup & system tray");
                ui.label(
                    RichText::new(
                        "Keep clipping available in the background without leaving the main window open.",
                    )
                    .size(12.0)
                    .color(MUTED),
                );
                ui.add_space(14.0);
                ui.checkbox(
                    &mut self.config.start_with_windows,
                    RichText::new("Start Xyra with Windows").strong(),
                );
                ui.add_space(6.0);
                ui.add_enabled_ui(self.config.start_with_windows, |ui| {
                    ui.checkbox(
                        &mut self.config.start_minimized_on_system_start,
                        "Start minimized in the system tray on Windows login",
                    );
                });
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.config.minimize_to_tray,
                    "Closing the window minimizes Xyra to the system tray",
                );
                ui.add_space(12.0);
                ui.label(
                    RichText::new(
                        "Tray controls: open Xyra, start or stop the replay buffer, save a clip, or quit completely.",
                    )
                    .size(11.0)
                    .color(CYAN),
                );
            });

        ui.add_space(14.0);
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(24)
            .show(ui, |ui| {
                section_title(ui, "Recording quality");
                ui.label(
                    RichText::new("Higher settings produce sharper clips and use more GPU and disk space.")
                        .size(12.0)
                        .color(MUTED),
                );
                ui.add_space(14.0);
                let mut chosen_quality = None;
                ui.columns(4, |columns| {
                    for (column, quality) in columns.iter_mut().zip(CaptureQuality::ALL) {
                        if quality_card(column, quality, self.config.quality == quality) {
                            chosen_quality = Some(quality);
                        }
                    }
                });
                if let Some(quality) = chosen_quality {
                    self.config.apply_quality(quality);
                }

                ui.add_space(20.0);
                ui.separator();
                ui.add_space(16.0);
                ui.columns(3, |columns| {
                    field_label(&mut columns[0], "VIDEO ENCODER");
                    egui::ComboBox::from_id_salt("video_encoder")
                        .selected_text(self.config.encoder.label())
                        .width(columns[0].available_width())
                        .show_ui(&mut columns[0], |ui| {
                            for encoder in EncoderBackend::ALL {
                                ui.selectable_value(
                                    &mut self.config.encoder,
                                    encoder,
                                    encoder.label(),
                                );
                            }
                        });

                    field_label(&mut columns[1], "CODEC");
                    egui::ComboBox::from_id_salt("video_codec")
                        .selected_text(self.config.video_codec.label())
                        .width(columns[1].available_width())
                        .show_ui(&mut columns[1], |ui| {
                            for codec in VideoCodec::ALL {
                                ui.selectable_value(
                                    &mut self.config.video_codec,
                                    codec,
                                    codec.label(),
                                );
                            }
                        });

                    field_label(&mut columns[2], "CAPTURE DISPLAY");
                    let current_monitor = selected_monitor(self.config.capture_monitor.as_deref());
                    let selected_text = current_monitor
                        .as_ref()
                        .map(MonitorInfo::display_label)
                        .unwrap_or_else(|| "No display found".into());
                    egui::ComboBox::from_id_salt("capture_monitor")
                        .selected_text(selected_text)
                        .width(columns[2].available_width())
                        .show_ui(&mut columns[2], |ui| {
                            for monitor in &self.monitors {
                                let selected = current_monitor
                                    .as_ref()
                                    .is_some_and(|current| current.id == monitor.id);
                                if ui
                                    .selectable_label(selected, monitor.display_label())
                                    .clicked()
                                {
                                    self.config.capture_monitor = Some(monitor.id.clone());
                                }
                            }
                        });
                });

                ui.add_space(16.0);
                if self.config.quality == CaptureQuality::Custom {
                    egui::Grid::new("custom_quality_grid")
                        .num_columns(2)
                        .min_col_width(190.0)
                        .spacing([30.0, 12.0])
                        .show(ui, |ui| {
                            field_label(ui, "OUTPUT WIDTH");
                            ui.add(
                                egui::DragValue::new(&mut self.config.output_width)
                                    .range(640..=7680)
                                    .speed(2),
                            );
                            ui.end_row();
                            field_label(ui, "OUTPUT HEIGHT");
                            ui.add(
                                egui::DragValue::new(&mut self.config.output_height)
                                    .range(360..=4320)
                                    .speed(2),
                            );
                            ui.end_row();
                            field_label(ui, "FRAME RATE");
                            ui.add(
                                egui::Slider::new(&mut self.config.frame_rate, 24..=240)
                                    .suffix(" fps"),
                            );
                            ui.end_row();
                            field_label(ui, "VIDEO BITRATE");
                            ui.add(
                                egui::Slider::new(&mut self.config.video_bitrate_mbps, 2..=100)
                                    .suffix(" Mbps"),
                            );
                            ui.end_row();
                        });
                } else {
                    let hardware_note = match self.config.encoder {
                        EncoderBackend::Software => "CPU encoding works on every PC",
                        _ => "Hardware encoding requires a compatible GPU and driver",
                    };
                    ui.label(
                        RichText::new(format!(
                            "{}×{} · {} FPS · {} Mbps   —   {hardware_note}",
                            self.config.output_width,
                            self.config.output_height,
                            self.config.frame_rate,
                            self.config.video_bitrate_mbps,
                        ))
                        .size(11.0)
                        .color(MUTED),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let monitor_count = self.monitors.len();
                    ui.label(
                        RichText::new(format!(
                            "Only the selected display is recorded · {monitor_count} display{} detected",
                            if monitor_count == 1 { "" } else { "s" }
                        ))
                        .size(11.0)
                        .color(CYAN),
                    );
                    if ui.small_button("Refresh displays").clicked() {
                        self.monitors = enumerate_monitors();
                    }
                });

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(16.0);
                ui.columns(2, |columns| {
                    section_title(&mut columns[0], "Video aspect ratio");
                    columns[0].label(
                        RichText::new(self.config.video_aspect_ratio.description())
                            .size(12.0)
                            .color(MUTED),
                    );
                    if self.config.encoder == EncoderBackend::Intel
                        && self.config.video_aspect_ratio == VideoAspectRatio::Game
                    {
                        columns[0].label(
                            RichText::new("Game Aspect Ratio can reduce performance on Intel GPUs.")
                                .size(11.0)
                                .color(WARNING),
                        );
                    }

                    columns[1].with_layout(
                        egui::Layout::right_to_left(egui::Align::TOP),
                        |ui| {
                            egui::ComboBox::from_id_salt("video_aspect_ratio")
                                .selected_text(self.config.video_aspect_ratio.label())
                                .width(270.0)
                                .show_ui(ui, |ui| {
                                    for mode in VideoAspectRatio::ALL {
                                        ui.selectable_value(
                                            &mut self.config.video_aspect_ratio,
                                            mode,
                                            mode.label(),
                                        );
                                    }
                                });
                        },
                    );
                });
            });

        ui.add_space(14.0);
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(24)
            .show(ui, |ui| {
                section_title(ui, "Audio recording");
                ui.label(
                    RichText::new(
                        "Capture desktop/game sound and your microphone, mixed or on separate tracks.",
                    )
                    .size(12.0)
                    .color(MUTED),
                );
                ui.add_space(16.0);

                let desktop_label = audio_device_label(
                    &self.audio_devices,
                    self.config.desktop_audio_device.as_deref(),
                    "No loopback device found",
                );
                egui::Frame::new()
                    .fill(SURFACE_2)
                    .stroke(egui::Stroke::new(
                        1.0,
                        if self.config.desktop_audio_enabled {
                            Color32::from_rgb(67, 57, 116)
                        } else {
                            BORDER
                        },
                    ))
                    .corner_radius(10)
                    .inner_margin(14)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut self.config.desktop_audio_enabled,
                                RichText::new("Desktop / game audio").strong(),
                            );
                            volume_control(
                                ui,
                                &mut self.config.desktop_audio_volume_percent,
                                self.config.desktop_audio_enabled,
                            );
                        });
                        ui.add_space(10.0);
                        field_label(ui, "WINDOWS AUDIO SOURCE");
                        ui.add_enabled_ui(self.config.desktop_audio_enabled, |ui| {
                            egui::ComboBox::from_id_salt("desktop_audio_device")
                                .selected_text(desktop_label)
                                .width(ui.available_width())
                                .show_ui(ui, |ui| {
                                    ui.label(
                                        RichText::new("DESKTOP / LOOPBACK SOURCES")
                                            .size(10.0)
                                            .color(CYAN),
                                    );
                                    for device in self
                                        .audio_devices
                                        .iter()
                                        .filter(|device| is_desktop_audio_device(device))
                                    {
                                        ui.selectable_value(
                                            &mut self.config.desktop_audio_device,
                                            Some(device.id.clone()),
                                            &device.name,
                                        );
                                    }
                                    let other_devices = self
                                        .audio_devices
                                        .iter()
                                        .filter(|device| !is_desktop_audio_device(device))
                                        .collect::<Vec<_>>();
                                    if !other_devices.is_empty() {
                                        ui.separator();
                                        ui.label(
                                            RichText::new(
                                                "OTHER DIRECTSHOW INPUTS (NOT SYSTEM AUDIO)",
                                            )
                                            .size(10.0)
                                            .color(MUTED),
                                        );
                                        ui.add_enabled_ui(false, |ui| {
                                            for device in other_devices {
                                                ui.label(format!("{}  ·  microphone/input", device.name));
                                            }
                                        });
                                    }
                                });
                        });
                    });

                ui.add_space(10.0);
                let microphone_label = audio_device_label(
                    &self.audio_devices,
                    self.config.microphone_device.as_deref(),
                    "No microphone found",
                );
                egui::Frame::new()
                    .fill(SURFACE_2)
                    .stroke(egui::Stroke::new(
                        1.0,
                        if self.config.microphone_enabled {
                            Color32::from_rgb(67, 57, 116)
                        } else {
                            BORDER
                        },
                    ))
                    .corner_radius(10)
                    .inner_margin(14)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(
                                &mut self.config.microphone_enabled,
                                RichText::new("Microphone").strong(),
                            );
                            volume_control(
                                ui,
                                &mut self.config.microphone_volume_percent,
                                self.config.microphone_enabled,
                            );
                        });
                        ui.add_space(10.0);
                        field_label(ui, "MICROPHONE SOURCE");
                        ui.add_enabled_ui(self.config.microphone_enabled, |ui| {
                            egui::ComboBox::from_id_salt("microphone_device")
                                .selected_text(microphone_label)
                                .width(ui.available_width())
                                .show_ui(ui, |ui| {
                                    ui.label(
                                        RichText::new("MICROPHONE SOURCES")
                                            .size(10.0)
                                            .color(CYAN),
                                    );
                                    for device in self
                                        .audio_devices
                                        .iter()
                                        .filter(|device| is_microphone_device(device))
                                    {
                                        ui.selectable_value(
                                            &mut self.config.microphone_device,
                                            Some(device.id.clone()),
                                            &device.name,
                                        );
                                    }
                                    let other_devices = self
                                        .audio_devices
                                        .iter()
                                        .filter(|device| !is_microphone_device(device))
                                        .collect::<Vec<_>>();
                                    if !other_devices.is_empty() {
                                        ui.separator();
                                        ui.label(
                                            RichText::new("OTHER DIRECTSHOW INPUTS")
                                                .size(10.0)
                                                .color(MUTED),
                                        );
                                        ui.add_enabled_ui(false, |ui| {
                                            for device in other_devices {
                                                ui.label(format!("{}  ·  desktop/loopback", device.name));
                                            }
                                        });
                                    }
                                });
                        });
                    });

                ui.add_space(14.0);
                ui.checkbox(
                    &mut self.config.separate_audio_tracks,
                    "Keep desktop and microphone on separate audio tracks",
                );
                ui.add_space(5.0);
                ui.columns(2, |columns| {
                    columns[0].checkbox(
                        &mut self.config.microphone_noise_suppression,
                        "Microphone noise suppression",
                    );
                    columns[1].checkbox(
                        &mut self.config.microphone_mono,
                        "Play mono microphone in both ears",
                    );
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} Windows audio endpoint{} detected: {} playback/loopback and {} microphone/input",
                            self.audio_devices.len(),
                            if self.audio_devices.len() == 1 { "" } else { "s" },
                            self.audio_devices
                                .iter()
                                .filter(|device| is_desktop_audio_device(device))
                                .count(),
                            self.audio_devices
                                .iter()
                                .filter(|device| !is_desktop_audio_device(device))
                                .count(),
                        ))
                        .size(11.0)
                        .color(if self.audio_devices.is_empty() {
                            WARNING
                        } else {
                            CYAN
                        }),
                    );
                    if ui.small_button("Refresh audio devices").clicked() {
                        self.refresh_audio_devices();
                    }
                });
                ui.label(
                    RichText::new(
                        "Playback devices use native Windows WASAPI loopback; microphone inputs use DirectShow.",
                    )
                    .size(11.0)
                    .color(MUTED),
                );
                if self.config.desktop_audio_device.is_none() {
                    ui.label(
                        RichText::new(
                            "Desktop audio needs Stereo Mix or a loopback/stream device such as SteelSeries Sonar or VB-Cable.",
                        )
                        .size(11.0)
                        .color(WARNING),
                    );
                }
            });

        ui.add_space(14.0);
        self.hotkeys_settings(ui);

        ui.add_space(14.0);
        egui::Frame::new()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(24)
            .show(ui, |ui| {
                section_title(ui, "Replay buffer & storage");
                ui.add_space(14.0);
                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .min_col_width(220.0)
                    .spacing([32.0, 18.0])
                    .show(ui, |ui| {
                        field_label(ui, "CLIP LENGTH");
                        ui.add(
                            egui::Slider::new(&mut self.config.clip_seconds, 5..=300)
                                .suffix(" sec"),
                        );
                        ui.end_row();
                        field_label(ui, "CLIPS FOLDER");
                        ui.label(
                            RichText::new(self.config.clips_directory.display().to_string())
                                .color(MUTED),
                        );
                        ui.end_row();
                        field_label(ui, "AUTO-QUEUE NEW CLIPS");
                        ui.checkbox(&mut self.config.auto_queue_after_clip, "Enabled");
                        ui.end_row();
                        field_label(ui, "FFMPEG EXECUTABLE");
                        ui.text_edit_singleline(&mut self.ffmpeg_input);
                        ui.end_row();
                    });
                ui.add_space(14.0);
                ui.label(
                    RichText::new(self.ffmpeg_installer.state().label())
                        .size(11.0)
                        .color(MUTED),
                );
                if let Some(progress) = self.ffmpeg_installer.state().progress() {
                    ui.add(
                        egui::ProgressBar::new(progress)
                            .desired_width(ui.available_width())
                            .animate(true),
                    );
                }
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_sized(
                            [150.0, 40.0],
                            egui::Button::new(RichText::new("Save settings").strong())
                                .fill(ACCENT)
                                .corner_radius(9),
                        )
                        .clicked()
                    {
                        let selected_path = PathBuf::from(self.ffmpeg_input.trim());
                        let managed = selected_path == managed_ffmpeg_path();
                        self.config.ffmpeg_path = selected_path.clone();
                        self.config.output_width = self.config.output_width.max(640) / 2 * 2;
                        self.config.output_height = self.config.output_height.max(360) / 2 * 2;
                        self.ffmpeg_ready = CaptureManager::ffmpeg_available(&self.config);
                        if selected_path != self.ffmpeg_installer.destination()
                            || !matches!(
                                self.ffmpeg_installer.state(),
                                InstallState::Starting
                                    | InstallState::Downloading { .. }
                                    | InstallState::Unpacking
                            )
                        {
                            self.ffmpeg_installer = FfmpegInstaller::new(selected_path, managed);
                        }
                        match self.config.save() {
                            Ok(()) => {
                                self.saved_config_state =
                                    serde_json::to_string(&self.config).unwrap_or_default();
                                self.config_save_due = None;
                                self.status =
                                    match sync_startup_registration(self.config.start_with_windows)
                                    {
                                        Ok(()) if self.ffmpeg_ready => {
                                            "Settings saved. FFmpeg is ready.".into()
                                        }
                                        Ok(()) => self.ffmpeg_installer.state().label(),
                                        Err(error) => format!(
                                            "Settings saved, but Windows startup failed: {error}"
                                        ),
                                    };
                            }
                            Err(error) => self.status = error.to_string(),
                        }
                    }

                    let managed_selected =
                        self.ffmpeg_installer.destination() == managed_ffmpeg_path();
                    let installing = matches!(
                        self.ffmpeg_installer.state(),
                        InstallState::Starting
                            | InstallState::Downloading { .. }
                            | InstallState::Unpacking
                    );
                    let show_managed_button = !managed_selected
                        || matches!(self.ffmpeg_installer.state(), InstallState::Failed(_));
                    if show_managed_button
                        && ui
                            .add_enabled(
                                !installing,
                                egui::Button::new(if managed_selected {
                                    "Retry bundled FFmpeg"
                                } else {
                                    "Use bundled FFmpeg"
                                })
                                .fill(SURFACE_2)
                                .corner_radius(9),
                            )
                            .clicked()
                    {
                        self.use_managed_ffmpeg();
                    }
                });
            });
        ui.add_space(14.0);
        egui::Frame::new()
            .fill(SURFACE_2)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(10)
            .inner_margin(14)
            .show(ui, |ui| {
                ui.label(RichText::new(&self.status).size(11.0).color(MUTED));
            });
    }

    fn hotkeys_settings(&mut self, ui: &mut egui::Ui) {
        const KEYS: [&str; 8] = ["F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12"];

        egui::Frame::new()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(14)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    section_title(ui, "Clip hotkeys");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("+ Add hotkey").clicked()
                            && self.config.clip_hotkeys.len() < KEYS.len()
                        {
                            let key = KEYS
                                .iter()
                                .find(|key| {
                                    !self.config.clip_hotkeys.iter().any(|hotkey| {
                                        hotkey.modifier == HotkeyModifier::None
                                            && hotkey.key == **key
                                    })
                                })
                                .copied()
                                .unwrap_or("F12");
                            self.config.clip_hotkeys.push(ClipHotkey::new(
                                key,
                                HotkeyModifier::None,
                                self.config.clip_seconds,
                            ));
                        }
                    });
                });
                ui.label(
                    RichText::new(
                        "Each shortcut saves a different amount of history from the same replay buffer, even while Xyra is in the background.",
                    )
                    .size(12.0)
                    .color(MUTED),
                );
                ui.add_space(14.0);

                let mut remove = None;
                for (index, hotkey) in self.config.clip_hotkeys.iter_mut().enumerate() {
                    egui::Frame::new()
                        .fill(SURFACE_2)
                        .stroke(egui::Stroke::new(1.0, BORDER))
                        .corner_radius(9)
                        .inner_margin(12)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut hotkey.enabled, "");
                                egui::ComboBox::from_id_salt(("hotkey_modifier", index))
                                    .selected_text(hotkey.modifier.label())
                                    .width(74.0)
                                    .show_ui(ui, |ui| {
                                        for modifier in HotkeyModifier::ALL {
                                            ui.selectable_value(
                                                &mut hotkey.modifier,
                                                modifier,
                                                modifier.label(),
                                            );
                                        }
                                    });
                                egui::ComboBox::from_id_salt(("hotkey_key", index))
                                    .selected_text(&hotkey.key)
                                    .width(68.0)
                                    .show_ui(ui, |ui| {
                                        for key in KEYS {
                                            ui.selectable_value(
                                                &mut hotkey.key,
                                                key.to_owned(),
                                                key,
                                            );
                                        }
                                    });
                                ui.label(RichText::new("Save previous").color(MUTED));
                                ui.add(
                                    egui::DragValue::new(&mut hotkey.clip_seconds)
                                        .range(5..=300)
                                        .speed(1)
                                        .suffix(" sec"),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("Remove").clicked() {
                                            remove = Some(index);
                                        }
                                    },
                                );
                            });
                        });
                    ui.add_space(7.0);
                }
                if let Some(index) = remove {
                    self.config.clip_hotkeys.remove(index);
                }

                ui.label(
                    RichText::new(&self.hotkey_status)
                        .size(11.0)
                        .color(if self.hotkey_status.contains("could not")
                            || self.hotkey_status.contains("more than once")
                        {
                            WARNING
                        } else {
                            SUCCESS
                        }),
                );
                ui.label(
                    RichText::new(format!(
                        "Replay history retained: {} sec. Restart a running replay buffer after increasing this value.",
                        self.config.max_buffer_seconds()
                    ))
                    .size(11.0)
                    .color(MUTED),
                );
            });
    }
}

impl eframe::App for XyraApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_ffmpeg_install();
        self.poll_player(ui.ctx());
        self.poll_hotkeys();
        self.poll_tray(ui.ctx());
        self.handle_window_close(ui.ctx());
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG))
            .show(ui, |ui| {
                let full = ui.max_rect();
                let sidebar_rect = egui::Rect::from_min_max(
                    full.min,
                    egui::pos2(
                        (full.left() + SIDEBAR_WIDTH).min(full.right()),
                        full.bottom(),
                    ),
                );
                let content_rect = egui::Rect::from_min_max(
                    egui::pos2(sidebar_rect.right(), full.top()),
                    full.max,
                );

                let mut sidebar_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(sidebar_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                self.sidebar(&mut sidebar_ui);

                let mut content_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(content_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                self.top_bar(&mut content_ui);
                egui::Frame::new()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(28, 24))
                    .show(&mut content_ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                            Page::Library => self.library_page(ui),
                            Page::Editor => self.editor_page(ui),
                            Page::Publish => self.publish_page(ui),
                            Page::Settings => self.settings_page(ui),
                        });
                    });
            });
        self.poll_config_auto_save(ui.ctx());
        ui.ctx().request_repaint_after(if self.player.is_playing() {
            std::time::Duration::from_millis(16)
        } else {
            std::time::Duration::from_millis(250)
        });
    }
}

impl Drop for XyraApp {
    fn drop(&mut self) {
        let _ = self.config.save();
    }
}

const BG: Color32 = Color32::from_rgb(10, 12, 17);
const SIDEBAR: Color32 = Color32::from_rgb(13, 16, 23);
const SURFACE: Color32 = Color32::from_rgb(20, 24, 34);
const SURFACE_2: Color32 = Color32::from_rgb(27, 32, 45);
const BORDER: Color32 = Color32::from_rgb(42, 47, 62);
const TEXT: Color32 = Color32::from_rgb(241, 243, 248);
const MUTED: Color32 = Color32::from_rgb(139, 146, 166);
const ACCENT: Color32 = Color32::from_rgb(112, 78, 255);
const ACCENT_LIGHT: Color32 = Color32::from_rgb(160, 137, 255);
const CYAN: Color32 = Color32::from_rgb(56, 202, 222);
const SUCCESS: Color32 = Color32::from_rgb(65, 207, 145);
const WARNING: Color32 = Color32::from_rgb(247, 177, 72);
const DANGER: Color32 = Color32::from_rgb(205, 65, 82);
const SIDEBAR_WIDTH: f32 = 212.0;

fn native_hotkey(configured: &ClipHotkey) -> Result<HotKey, String> {
    let text = match configured.modifier {
        HotkeyModifier::None => configured.key.clone(),
        modifier => format!("{}+{}", modifier.label(), configured.key),
    };
    text.parse::<HotKey>()
        .map_err(|error| format!("Invalid hotkey {text}: {error}"))
}

fn sync_audio_device_selection(config: &mut AppConfig, devices: &[AudioDevice]) {
    let desktop_is_valid = config.desktop_audio_device.as_ref().is_some_and(|id| {
        devices
            .iter()
            .any(|device| &device.id == id && is_desktop_audio_device(device))
    });
    if !desktop_is_valid {
        config.desktop_audio_device =
            recommended_desktop_audio(devices).map(|device| device.id.clone());
        if config.desktop_audio_device.is_none() {
            config.desktop_audio_enabled = false;
        }
    }

    let microphone_is_valid = config.microphone_device.as_ref().is_some_and(|id| {
        devices
            .iter()
            .any(|device| &device.id == id && is_microphone_device(device))
    });
    if !microphone_is_valid {
        config.microphone_device = recommended_microphone(devices).map(|device| device.id.clone());
        if config.microphone_device.is_none() {
            config.microphone_enabled = false;
        }
    }
}

fn audio_device_label(devices: &[AudioDevice], selected: Option<&str>, fallback: &str) -> String {
    selected
        .and_then(|id| devices.iter().find(|device| device.id == id))
        .map(|device| device.name.clone())
        .unwrap_or_else(|| fallback.to_owned())
}

fn ffmpeg_status(state: &InstallState, ready: bool) -> (String, Color32) {
    if ready {
        return ("Ready".into(), SUCCESS);
    }
    match state {
        InstallState::Starting | InstallState::Downloading { .. } | InstallState::Unpacking => {
            (state.label(), CYAN)
        }
        InstallState::Failed(_) => ("Setup failed".into(), DANGER),
        InstallState::NotManaged | InstallState::Ready => ("Setup required".into(), WARNING),
    }
}

fn engine_badge(ui: &mut egui::Ui, running: bool, state: &InstallState, ready: bool) {
    let (text, color) = if running {
        ("Recording".into(), DANGER)
    } else {
        ffmpeg_status(state, ready)
    };
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(20)
        .inner_margin(egui::Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(7.0, 7.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, color);
                ui.label(RichText::new(text).size(11.0).strong().color(TEXT));
            });
        });
}

fn nav_button(ui: &mut egui::Ui, current: &mut Page, page: Page, text: &str) {
    let selected = *current == page;
    if ui
        .add_sized(
            [ui.available_width(), 42.0],
            egui::Button::new(RichText::new(text).size(13.0).strong().color(if selected {
                Color32::WHITE
            } else {
                MUTED
            }))
            .fill(if selected {
                Color32::from_rgb(42, 34, 80)
            } else {
                Color32::TRANSPARENT
            })
            .stroke(if selected {
                egui::Stroke::new(1.0, Color32::from_rgb(77, 59, 141))
            } else {
                egui::Stroke::NONE
            })
            .corner_radius(9),
        )
        .clicked()
    {
        *current = page;
    }
    ui.add_space(5.0);
}

fn page_heading(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.heading(RichText::new(title).size(26.0).color(TEXT));
    ui.label(RichText::new(subtitle).size(12.0).color(MUTED));
    ui.add_space(22.0);
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).size(17.0).strong().color(TEXT));
}

fn field_label(ui: &mut egui::Ui, label: &str) {
    ui.label(RichText::new(label).size(10.0).strong().color(MUTED));
}

fn volume_control(ui: &mut egui::Ui, value: &mut u32, enabled: bool) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        egui::Frame::new()
            .fill(Color32::from_rgb(35, 40, 54))
            .corner_radius(7)
            .inner_margin(egui::Margin::symmetric(10, 5))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(format!("{}%", *value))
                        .size(11.0)
                        .color(if enabled { TEXT } else { MUTED }),
                );
            });

        let sense = if enabled {
            egui::Sense::click_and_drag()
        } else {
            egui::Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(egui::vec2(145.0, 22.0), sense);
        if enabled
            && (response.clicked() || response.dragged())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let fraction = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            *value = (fraction * 200.0).round() as u32;
        }

        let center_y = rect.center().y;
        let fraction = (*value).min(200) as f32 / 200.0;
        let track_color = if enabled {
            Color32::from_rgb(61, 67, 82)
        } else {
            Color32::from_rgb(43, 47, 58)
        };
        ui.painter().line_segment(
            [
                egui::pos2(rect.left(), center_y),
                egui::pos2(rect.right(), center_y),
            ],
            egui::Stroke::new(5.0, track_color),
        );
        let handle_x = rect.left() + rect.width() * fraction;
        if handle_x > rect.left() {
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left(), center_y),
                    egui::pos2(handle_x, center_y),
                ],
                egui::Stroke::new(5.0, if enabled { ACCENT } else { MUTED }),
            );
        }
        ui.painter().circle_filled(
            egui::pos2(handle_x, center_y),
            7.0,
            if enabled { ACCENT_LIGHT } else { MUTED },
        );
        if response.hovered() && enabled {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        field_label(ui, "VOLUME");
    });
}

fn quality_card(ui: &mut egui::Ui, quality: CaptureQuality, selected: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 92.0), egui::Sense::click());
    let fill = if selected {
        Color32::from_rgb(45, 39, 71)
    } else if response.hovered() {
        Color32::from_rgb(29, 34, 47)
    } else {
        SURFACE_2
    };
    ui.painter().rect(
        rect,
        10.0,
        fill,
        egui::Stroke::new(
            if selected { 1.5 } else { 1.0 },
            if selected { ACCENT_LIGHT } else { BORDER },
        ),
        egui::StrokeKind::Inside,
    );
    let mut card = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(14.0))
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    card.horizontal(|ui| {
        ui.label(
            RichText::new(quality.label())
                .size(14.0)
                .strong()
                .color(TEXT),
        );
        if selected {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, ACCENT_LIGHT);
            });
        }
    });
    card.add_space(8.0);
    card.label(RichText::new(quality.description()).size(11.0).color(MUTED));
    response.clicked()
}

fn format_time(seconds: f32) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

fn metric_card(
    ui: &mut egui::Ui,
    label: &str,
    value: &str,
    detail: &str,
    color: Color32,
    width: f32,
) {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(12)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(width - 32.0);
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(label).size(9.0).strong().color(MUTED));
                    ui.add_space(4.0);
                    ui.label(RichText::new(value).size(24.0).strong().color(TEXT));
                    ui.label(RichText::new(detail).size(10.0).color(MUTED));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 6.0, color);
                });
            });
        });
}

fn empty_state(ui: &mut egui::Ui, title: &str, detail: &str, show_action_hint: bool) {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(16)
        .inner_margin(24)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(300.0);
            ui.vertical_centered(|ui| {
                ui.add_space(58.0);
                let (icon, _) =
                    ui.allocate_exact_size(egui::vec2(60.0, 60.0), egui::Sense::hover());
                ui.painter()
                    .circle_filled(icon.center(), 30.0, Color32::from_rgb(42, 34, 80));
                ui.painter().text(
                    icon.center(),
                    egui::Align2::CENTER_CENTER,
                    "REC",
                    egui::FontId::proportional(11.0),
                    ACCENT_LIGHT,
                );
                ui.add_space(16.0);
                ui.label(RichText::new(title).size(20.0).strong().color(TEXT));
                ui.label(RichText::new(detail).size(12.0).color(MUTED));
                if show_action_hint {
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new("Use Start capture in the top-right corner")
                            .size(11.0)
                            .color(ACCENT_LIGHT),
                    );
                }
            });
        });
}

fn clip_card(ui: &mut egui::Ui, clip: &Clip, selected: bool) -> Option<ClipCardAction> {
    let mut play_clicked = false;
    let mut reveal_clicked = false;
    let frame = egui::Frame::new()
        .fill(if selected {
            Color32::from_rgb(29, 27, 48)
        } else {
            SURFACE
        })
        .stroke(egui::Stroke::new(
            1.0,
            if selected { ACCENT } else { BORDER },
        ))
        .corner_radius(13)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let (preview, play_response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), 118.0),
                egui::Sense::click(),
            );
            ui.painter()
                .rect_filled(preview, 9.0, Color32::from_rgb(8, 10, 15));
            ui.painter().circle_filled(
                preview.center(),
                if play_response.hovered() { 25.0 } else { 23.0 },
                if play_response.hovered() {
                    ACCENT_LIGHT
                } else {
                    Color32::from_rgba_unmultiplied(112, 78, 255, 215)
                },
            );
            ui.painter().text(
                preview.center(),
                egui::Align2::CENTER_CENTER,
                "PLAY",
                egui::FontId::proportional(9.0),
                Color32::WHITE,
            );
            if play_response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            play_response.context_menu(|ui| {
                clip_context_menu_contents(ui, clip, &mut reveal_clicked);
            });
            play_clicked = play_response.clicked();
            ui.add_space(10.0);
            ui.label(RichText::new(&clip.title).size(14.0).strong().color(TEXT));
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(clip.created_at.format("%b %d, %H:%M").to_string())
                        .size(10.0)
                        .color(MUTED),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{:.0}s", clip.duration_secs))
                            .size(10.0)
                            .strong()
                            .color(ACCENT_LIGHT),
                    );
                });
            });
        });
    let card_response = frame.response.interact(egui::Sense::click());
    card_response.context_menu(|ui| {
        clip_context_menu_contents(ui, clip, &mut reveal_clicked);
    });
    if card_response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if reveal_clicked {
        Some(ClipCardAction::RevealInExplorer)
    } else if play_clicked {
        Some(ClipCardAction::Play)
    } else if card_response.clicked() {
        Some(ClipCardAction::OpenEditor)
    } else {
        None
    }
}

fn clip_context_menu_contents(ui: &mut egui::Ui, clip: &Clip, reveal_clicked: &mut bool) {
    ui.set_min_width(190.0);
    ui.label(RichText::new(&clip.title).size(11.0).color(MUTED));
    ui.separator();
    if ui.button("Show in File Explorer").clicked() {
        *reveal_clicked = true;
        ui.close();
    }
}

fn reveal_in_file_manager(path: &std::path::Path) -> std::io::Result<()> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    #[cfg(windows)]
    {
        std::process::Command::new("explorer.exe")
            .arg("/select,")
            .arg(path)
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let directory = path.parent().unwrap_or(&path);
        std::process::Command::new("xdg-open")
            .arg(directory)
            .spawn()?;
    }
    Ok(())
}

fn export_path(input: &std::path::Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("clip");
    input.with_file_name(format!("{stem}-edited.mp4"))
}

fn configure_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = BG;
    visuals.extreme_bg_color = Color32::from_rgb(12, 15, 22);
    visuals.faint_bg_color = SURFACE;
    visuals.widgets.inactive.bg_fill = SURFACE_2;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, BORDER);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(50, 43, 82);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT);
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.selection.bg_fill = Color32::from_rgb(74, 54, 156);
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT_LIGHT);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size.y = 34.0;
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(8);
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(8);
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(8);
    ctx.set_style_of(egui::Theme::Dark, style);
}
