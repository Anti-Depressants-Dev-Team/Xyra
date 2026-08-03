#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    ToggleReplayBuffer,
    SaveClip,
    Quit,
}

#[cfg(windows)]
pub struct SystemTray {
    _icon: tray_icon::TrayIcon,
    status_item: tray_icon::menu::MenuItem,
    capture_item: tray_icon::menu::MenuItem,
    save_item: tray_icon::menu::MenuItem,
    receiver: std::sync::mpsc::Receiver<TrayAction>,
    was_running: Option<bool>,
    clip_seconds: u32,
}

#[cfg(windows)]
impl SystemTray {
    pub fn new(ctx: &egui::Context) -> Result<Self, String> {
        use tray_icon::{
            Icon, TrayIconBuilder, TrayIconEvent,
            menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        };

        const OPEN_ID: &str = "xyra.tray.open";
        const CAPTURE_ID: &str = "xyra.tray.capture";
        const SAVE_ID: &str = "xyra.tray.save";
        const QUIT_ID: &str = "xyra.tray.quit";

        let open_item = MenuItem::with_id(OPEN_ID, "Open Xyra", true, None);
        let status_item = MenuItem::new("Replay buffer: Stopped", false, None);
        let capture_item = MenuItem::with_id(CAPTURE_ID, "Start replay buffer", true, None);
        let save_item = MenuItem::with_id(SAVE_ID, "Save last 30 seconds", false, None);
        let quit_item = MenuItem::with_id(QUIT_ID, "Quit Xyra", true, None);
        let separator_one = PredefinedMenuItem::separator();
        let separator_two = PredefinedMenuItem::separator();
        let menu = Menu::with_items(&[
            &open_item,
            &separator_one,
            &status_item,
            &capture_item,
            &save_item,
            &separator_two,
            &quit_item,
        ])
        .map_err(|error| error.to_string())?;
        let icon = Icon::from_rgba(tray_icon_rgba(), 32, 32).map_err(|error| error.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_tooltip("Xyra — Replay buffer stopped")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .map_err(|error| error.to_string())?;

        let (sender, receiver) = std::sync::mpsc::channel();
        let menu_sender = sender.clone();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let action = match event.id.0.as_str() {
                OPEN_ID => Some(TrayAction::Show),
                CAPTURE_ID => Some(TrayAction::ToggleReplayBuffer),
                SAVE_ID => Some(TrayAction::SaveClip),
                QUIT_ID => Some(TrayAction::Quit),
                _ => None,
            };
            if let Some(action) = action {
                let _ = menu_sender.send(action);
                menu_ctx.request_repaint();
            }
        }));
        let tray_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let open = matches!(
                event,
                TrayIconEvent::DoubleClick {
                    button: tray_icon::MouseButton::Left,
                    ..
                } | TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                }
            );
            if open {
                let _ = sender.send(TrayAction::Show);
                tray_ctx.request_repaint();
            }
        }));

        Ok(Self {
            _icon: icon,
            status_item,
            capture_item,
            save_item,
            receiver,
            was_running: None,
            clip_seconds: 0,
        })
    }

    pub fn drain_actions(&self) -> Vec<TrayAction> {
        self.receiver.try_iter().collect()
    }

    pub fn update(&mut self, running: bool, clip_seconds: u32) {
        if self.was_running == Some(running) && self.clip_seconds == clip_seconds {
            return;
        }
        self.was_running = Some(running);
        self.clip_seconds = clip_seconds;
        self.status_item.set_text(if running {
            "Replay buffer: Recording"
        } else {
            "Replay buffer: Stopped"
        });
        self.capture_item.set_text(if running {
            "Stop replay buffer"
        } else {
            "Start replay buffer"
        });
        self.save_item
            .set_text(format!("Save last {} seconds", clip_seconds.max(5)));
        self.save_item.set_enabled(running);
        let _ = self._icon.set_tooltip(Some(if running {
            "Xyra — Replay buffer recording"
        } else {
            "Xyra — Replay buffer stopped"
        }));
    }
}

#[cfg(windows)]
fn tray_icon_rgba() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0_i32..32 {
        for x in 0_i32..32 {
            let inside = (x - 16).pow(2) + (y - 16).pow(2) <= 15_i32.pow(2);
            let cross = (x - y).abs() <= 2 || (x + y - 31).abs() <= 2;
            let pixel = if inside && cross {
                [245, 243, 255, 255]
            } else if inside {
                [112, 78, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    rgba
}

#[cfg(not(windows))]
pub struct SystemTray;

#[cfg(not(windows))]
impl SystemTray {
    pub fn new(_ctx: &egui::Context) -> Result<Self, String> {
        Err("The Xyra tray is currently supported on Windows".into())
    }

    pub fn drain_actions(&self) -> Vec<TrayAction> {
        Vec::new()
    }

    pub fn update(&mut self, _running: bool, _clip_seconds: u32) {}
}
