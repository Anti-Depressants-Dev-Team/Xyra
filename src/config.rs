use std::{
    fs,
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{Platform, PublishTarget};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureEngine {
    Auto,
    Obs,
    Ffmpeg,
}

impl CaptureEngine {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Obs, Self::Ffmpeg];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic (OBS preferred)",
            Self::Obs => "OBS engine",
            Self::Ffmpeg => "Legacy FFmpeg",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => {
                "Uses the bundled OBS engine and falls back to FFmpeg if OBS cannot start."
            }
            Self::Obs => "GPU-native OBS capture, audio mixing, encoding, and replay buffering.",
            Self::Ffmpeg => "Compatibility backend retained for troubleshooting.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureQuality {
    Low,
    Standard,
    High,
    Custom,
}

impl CaptureQuality {
    pub const ALL: [Self; 4] = [Self::Low, Self::Standard, Self::High, Self::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "Low quality",
            Self::Standard => "Standard",
            Self::High => "High quality",
            Self::Custom => "Custom",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Low => "720p 30 FPS · 5 Mbps",
            Self::Standard => "1080p 60 FPS · 12 Mbps",
            Self::High => "1080p 60 FPS · 24 Mbps",
            Self::Custom => "Choose every setting",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncoderBackend {
    Auto,
    Software,
    Nvidia,
    Amd,
    Intel,
}

impl EncoderBackend {
    pub const ALL: [Self; 5] = [
        Self::Auto,
        Self::Nvidia,
        Self::Amd,
        Self::Intel,
        Self::Software,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic (recommended)",
            Self::Software => "CPU (software)",
            Self::Nvidia => "NVIDIA NVENC",
            Self::Amd => "AMD AMF",
            Self::Intel => "Intel Quick Sync",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    H265,
}

impl VideoCodec {
    pub const ALL: [Self; 2] = [Self::H264, Self::H265];

    pub fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264 (compatible)",
            Self::H265 => "H.265 (smaller files)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoAspectRatio {
    Stretch16By9,
    Fit16By9,
    Game,
    Crop16By9,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipHotkey {
    pub enabled: bool,
    pub key: String,
    pub modifier: HotkeyModifier,
    pub clip_seconds: u32,
}

impl ClipHotkey {
    pub fn new(key: impl Into<String>, modifier: HotkeyModifier, clip_seconds: u32) -> Self {
        Self {
            enabled: true,
            key: key.into(),
            modifier,
            clip_seconds,
        }
    }

    pub fn label(&self) -> String {
        let key = if self.modifier == HotkeyModifier::None {
            self.key.clone()
        } else {
            format!("{} + {}", self.modifier.label(), self.key)
        };
        format!("{key} saves the previous {} sec", self.clip_seconds)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HotkeyModifier {
    None,
    Control,
    Alt,
    Shift,
}

impl HotkeyModifier {
    pub const ALL: [Self; 4] = [Self::None, Self::Control, Self::Alt, Self::Shift];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Control => "Ctrl",
            Self::Alt => "Alt",
            Self::Shift => "Shift",
        }
    }
}

impl VideoAspectRatio {
    pub const ALL: [Self; 4] = [
        Self::Stretch16By9,
        Self::Fit16By9,
        Self::Game,
        Self::Crop16By9,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Stretch16By9 => "Stretch to Fit 16:9",
            Self::Fit16By9 => "Fit 16:9 with Black Bars",
            Self::Game => "Game Aspect Ratio",
            Self::Crop16By9 => "Crop to 16:9",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Stretch16By9 => {
                "Fills the whole 16:9 frame; ultrawide footage may look stretched."
            }
            Self::Fit16By9 => "Keeps the original proportions and adds black bars when needed.",
            Self::Game => "Keeps your selected display's aspect ratio without bars or cropping.",
            Self::Crop16By9 => "Fills a 16:9 frame and trims overflow from the center.",
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine an application data directory")]
    NoDataDirectory,
    #[error("could not read or write configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("configuration is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub config_version: u32,
    pub capture_engine: CaptureEngine,
    pub ffmpeg_path: PathBuf,
    pub start_with_windows: bool,
    pub start_minimized_on_system_start: bool,
    pub minimize_to_tray: bool,
    pub clip_seconds: u32,
    pub clip_hotkeys: Vec<ClipHotkey>,
    pub segment_seconds: u32,
    pub frame_rate: u32,
    pub quality: CaptureQuality,
    pub encoder: EncoderBackend,
    pub video_codec: VideoCodec,
    pub video_aspect_ratio: VideoAspectRatio,
    pub video_bitrate_mbps: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub capture_cursor: bool,
    pub animated_cursor_compatibility: bool,
    /// Windows display device name, such as `\\.\DISPLAY1`. None selects the primary display.
    pub capture_monitor: Option<String>,
    pub desktop_audio_enabled: bool,
    pub desktop_audio_device: Option<String>,
    pub desktop_audio_volume_percent: u32,
    pub microphone_enabled: bool,
    pub microphone_device: Option<String>,
    pub microphone_volume_percent: u32,
    pub separate_audio_tracks: bool,
    pub microphone_noise_suppression: bool,
    pub microphone_mono: bool,
    pub clips_directory: PathBuf,
    pub buffer_directory: PathBuf,
    pub auto_queue_after_clip: bool,
    pub publish_targets: Vec<PublishTarget>,
    pub youtube_client_id: String,
    pub odysee_api_url: String,
    pub odysee_bid: String,
    pub odysee_channel_id: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        let base = project_dirs()
            .map(|dirs| dirs.data_local_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".xyra"));
        Self {
            config_version: 3,
            capture_engine: CaptureEngine::Auto,
            ffmpeg_path: managed_ffmpeg_path_from(&base),
            start_with_windows: false,
            start_minimized_on_system_start: true,
            minimize_to_tray: true,
            clip_seconds: 30,
            clip_hotkeys: vec![
                ClipHotkey::new("F8", HotkeyModifier::None, 30),
                ClipHotkey::new("F7", HotkeyModifier::None, 60),
                ClipHotkey::new("F6", HotkeyModifier::None, 120),
            ],
            segment_seconds: 2,
            frame_rate: 60,
            quality: CaptureQuality::Standard,
            encoder: EncoderBackend::Auto,
            video_codec: VideoCodec::H264,
            video_aspect_ratio: VideoAspectRatio::Stretch16By9,
            video_bitrate_mbps: 12,
            output_width: 1920,
            output_height: 1080,
            capture_cursor: true,
            animated_cursor_compatibility: false,
            capture_monitor: None,
            desktop_audio_enabled: true,
            desktop_audio_device: None,
            desktop_audio_volume_percent: 100,
            microphone_enabled: true,
            microphone_device: None,
            microphone_volume_percent: 80,
            separate_audio_tracks: true,
            microphone_noise_suppression: false,
            microphone_mono: false,
            clips_directory: base.join("clips"),
            buffer_directory: base.join("buffer"),
            auto_queue_after_clip: false,
            publish_targets: Platform::ALL.into_iter().map(PublishTarget::new).collect(),
            youtube_client_id: String::new(),
            odysee_api_url: "http://127.0.0.1:5279".into(),
            odysee_bid: "0.01".into(),
            odysee_channel_id: String::new(),
        }
    }
}

impl AppConfig {
    pub fn max_buffer_seconds(&self) -> u32 {
        self.clip_hotkeys
            .iter()
            .filter(|hotkey| hotkey.enabled)
            .map(|hotkey| hotkey.clip_seconds)
            .chain(std::iter::once(self.clip_seconds))
            .max()
            .unwrap_or(self.clip_seconds)
            .max(5)
    }

    pub fn apply_quality(&mut self, quality: CaptureQuality) {
        self.quality = quality;
        match quality {
            CaptureQuality::Low => {
                self.output_width = 1280;
                self.output_height = 720;
                self.frame_rate = 30;
                self.video_bitrate_mbps = 5;
            }
            CaptureQuality::Standard => {
                self.output_width = 1920;
                self.output_height = 1080;
                self.frame_rate = 60;
                self.video_bitrate_mbps = 12;
            }
            CaptureQuality::High => {
                self.output_width = 1920;
                self.output_height = 1080;
                self.frame_rate = 60;
                self.video_bitrate_mbps = 24;
            }
            CaptureQuality::Custom => {}
        }
    }

    pub fn load() -> Result<Self, ConfigError> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)?;
        let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
        let mut config: Self = serde_json::from_slice(&bytes)?;
        // The first alpha defaulted to CPU encoding even on systems with a
        // dedicated encoder. Migrate only those unversioned configs; explicit
        // choices made after automatic detection was added remain untouched.
        if raw.get("config_version").is_none() {
            if config.encoder == EncoderBackend::Software {
                config.encoder = EncoderBackend::Auto;
            }
            config.config_version = 2;
        }
        if config.config_version < 3 {
            config.capture_engine = CaptureEngine::Auto;
            config.config_version = 3;
        }
        config.frame_rate = config.frame_rate.clamp(24, 120);
        config.video_bitrate_mbps = config.video_bitrate_mbps.clamp(2, 100);
        config.segment_seconds = config.segment_seconds.clamp(1, 10);
        // Migrate early Xyra installs that expected FFmpeg to exist on PATH.
        if config.ffmpeg_path == Path::new("ffmpeg")
            || config.ffmpeg_path == Path::new("ffmpeg.exe")
        {
            config.ffmpeg_path = managed_ffmpeg_path();
        }
        Ok(config)
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(&self.clips_directory)?;
        fs::create_dir_all(&self.buffer_directory)?;
        fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

pub fn managed_ffmpeg_path() -> PathBuf {
    let base = project_dirs()
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".xyra"));
    managed_ffmpeg_path_from(&base)
}

fn managed_ffmpeg_path_from(base: &std::path::Path) -> PathBuf {
    let mut path = base.join("runtime").join("ffmpeg");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("tv", "Xyra", "Xyra")
}

pub fn config_path() -> Result<PathBuf, ConfigError> {
    let dirs = project_dirs().ok_or(ConfigError::NoDataDirectory)?;
    Ok(dirs.config_dir().join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_presets_update_all_capture_values() {
        let mut config = AppConfig::default();
        config.apply_quality(CaptureQuality::Low);
        assert_eq!((config.output_width, config.output_height), (1280, 720));
        assert_eq!(config.frame_rate, 30);
        assert_eq!(config.video_bitrate_mbps, 5);

        config.apply_quality(CaptureQuality::Standard);
        assert_eq!((config.output_width, config.output_height), (1920, 1080));
        assert_eq!(config.frame_rate, 60);
        assert_eq!(config.video_bitrate_mbps, 12);
    }

    #[test]
    fn longest_enabled_hotkey_controls_buffer_length() {
        let mut config = AppConfig {
            clip_seconds: 30,
            ..AppConfig::default()
        };
        config.clip_hotkeys[2].clip_seconds = 180;
        assert_eq!(config.max_buffer_seconds(), 180);
        config.clip_hotkeys[2].enabled = false;
        assert_eq!(config.max_buffer_seconds(), 60);
    }
}
