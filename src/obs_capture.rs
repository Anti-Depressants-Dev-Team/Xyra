use std::path::Path;

#[cfg(not(windows))]
use std::path::PathBuf;

use thiserror::Error;

use crate::{
    config::{AppConfig, EncoderBackend, VideoAspectRatio, VideoCodec},
    model::Clip,
};

pub const OBS_RUNTIME_VERSION: &str = "32.2.1";

#[derive(Debug, Error)]
pub enum ObsCaptureError {
    #[error("the bundled OBS {OBS_RUNTIME_VERSION} runtime is missing or incomplete")]
    RuntimeMissing,
    #[error("OBS capture is only supported on Windows in this alpha")]
    UnsupportedPlatform,
    #[error("OBS engine failed: {0}")]
    Engine(String),
    #[error("could not finalize the OBS replay: {0}")]
    Replay(String),
}

#[cfg(windows)]
mod platform {
    use std::{fs, path::PathBuf};

    use chrono::Utc;
    use libobs_simple::{
        output::{
            replay::ObsContextReplayExt,
            simple::{HardwareCodec, HardwarePreset, X264Preset},
        },
        sources::windows::{MonitorCaptureSourceBuilder, ObsDisplayCaptureMethod},
    };
    use libobs_wrapper::{
        context::ObsContext,
        data::{
            ObsDataSetters,
            object::ObsObjectTrait,
            output::{ObsOutputTrait, ObsReplayBufferOutputRef},
            video::ObsVideoInfoBuilder,
        },
        encoders::ObsAudioEncoderType,
        enums::ObsBoundsType,
        graphics::Vec2,
        run_with_obs,
        scenes::{ObsTransformInfoBuilder, SceneItemExtSceneTrait, SceneItemTrait},
        sources::{ObsSourceBuilder, ObsSourceRef},
        utils::{AudioEncoderInfo, FilterInfo, ObsPath, SourceInfo, StartupInfo, StartupPaths},
    };

    use super::{
        AppConfig, Clip, EncoderBackend, ObsCaptureError, Path, VideoAspectRatio, VideoCodec,
    };
    use crate::{
        process::hidden_command,
        windows_audio::{wasapi_capture_endpoint_id, wasapi_endpoint_id},
    };

    pub struct ObsCaptureEngine {
        context: ObsContext,
        replay: ObsReplayBufferOutputRef,
        active_encoder: EncoderBackend,
        max_buffer_seconds: u32,
    }

    impl std::fmt::Debug for ObsCaptureEngine {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ObsCaptureEngine")
                .field("active_encoder", &self.active_encoder)
                .field("max_buffer_seconds", &self.max_buffer_seconds)
                .finish_non_exhaustive()
        }
    }

    #[derive(Clone)]
    pub struct ObsReplayRequest {
        _context: ObsContext,
        replay: ObsReplayBufferOutputRef,
        ffmpeg_path: PathBuf,
        requested_seconds: u32,
        max_buffer_seconds: u32,
    }

    impl ObsCaptureEngine {
        pub fn start(config: &AppConfig) -> Result<Self, ObsCaptureError> {
            let runtime = find_runtime_root().ok_or(ObsCaptureError::RuntimeMissing)?;
            configure_dll_search_path(&runtime)?;
            ensure_helper_binaries(&runtime)?;
            fs::create_dir_all(&config.clips_directory)
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;

            let libobs_data = runtime.join("data").join("libobs");
            let curated_plugin_bin = runtime.join("xyra-plugins").join("64bit");
            let plugin_bin = if curated_plugin_bin.is_dir() {
                curated_plugin_bin
            } else {
                runtime.join("obs-plugins").join("64bit")
            };
            let plugin_data = runtime.join("data").join("obs-plugins").join("%module%");
            let startup_paths = StartupPaths::new(
                ObsPath::new(&libobs_data.to_string_lossy()),
                ObsPath::new(&plugin_bin.to_string_lossy()),
                ObsPath::new(&plugin_data.to_string_lossy()),
            );

            let monitor = crate::display::selected_monitor(config.capture_monitor.as_deref())
                .ok_or_else(|| ObsCaptureError::Engine("no Windows display was found".into()))?;
            let (output_width, output_height) = obs_output_dimensions(config, &monitor);
            let video_info = ObsVideoInfoBuilder::new()
                .fps_num(config.frame_rate)
                .fps_den(1)
                .base_width(monitor.width)
                .base_height(monitor.height)
                .output_width(output_width)
                .output_height(output_height)
                .build();
            let startup = StartupInfo::new()
                .set_startup_paths(startup_paths)
                .set_video_info(video_info);
            // win-capture resolves its signed hook payload from ../../data during module
            // initialization, matching OBS Studio's bin/64bit working directory layout.
            let context_result = if runtime.join("xyra-plugins").is_dir() {
                let previous_directory = std::env::current_dir()
                    .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
                let obs_working_directory = runtime.join("bin").join("64bit");
                fs::create_dir_all(&obs_working_directory)
                    .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
                std::env::set_current_dir(&obs_working_directory)
                    .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
                let result = ObsContext::new(startup);
                std::env::set_current_dir(previous_directory).map_err(|error| {
                    ObsCaptureError::Engine(format!(
                        "could not restore Xyra's working directory: {error}"
                    ))
                })?;
                result
            } else {
                ObsContext::new(startup)
            };
            let mut context =
                context_result.map_err(|error| ObsCaptureError::Engine(error.to_string()))?;

            let mut scene = context
                .scene("xyra-replay-scene", Some(0))
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
            let obs_monitors = MonitorCaptureSourceBuilder::get_monitors()
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
            let selected = obs_monitors
                .iter()
                .find(|candidate| candidate.0.name.eq_ignore_ascii_case(&monitor.id))
                .or_else(|| obs_monitors.iter().find(|candidate| candidate.0.is_primary))
                .or_else(|| obs_monitors.first())
                .ok_or_else(|| ObsCaptureError::Engine("OBS did not report a display".into()))?;
            let monitor_item = context
                .source_builder::<MonitorCaptureSourceBuilder, _>("xyra-monitor")
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?
                .set_monitor(selected)
                .set_capture_cursor(config.capture_cursor)
                .set_compatibility(config.animated_cursor_compatibility)
                .set_capture_method(ObsDisplayCaptureMethod::MethodAuto)
                .add_to_scene(&mut scene)
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
            apply_video_transform(
                &monitor_item,
                config.video_aspect_ratio,
                output_width,
                output_height,
            )?;

            let separate_tracks = config.separate_audio_tracks
                && config.desktop_audio_enabled
                && config.microphone_enabled;
            if config.desktop_audio_enabled {
                let endpoint = config
                    .desktop_audio_device
                    .as_deref()
                    .and_then(wasapi_endpoint_id)
                    .unwrap_or("default");
                add_audio_source(
                    &mut context,
                    &mut scene,
                    AudioSourceSpec {
                        source_id: "wasapi_output_capture",
                        source_name: "xyra-desktop-audio",
                        endpoint_id: endpoint,
                        volume_percent: config.desktop_audio_volume_percent,
                        mixers: if separate_tracks { 0b011 } else { 0b001 },
                        noise_suppression: false,
                    },
                )?;
            }
            if config.microphone_enabled {
                let endpoint = config
                    .microphone_device
                    .as_deref()
                    .and_then(wasapi_capture_endpoint_id)
                    .unwrap_or("default");
                add_audio_source(
                    &mut context,
                    &mut scene,
                    AudioSourceSpec {
                        source_id: "wasapi_input_capture",
                        source_name: "xyra-microphone",
                        endpoint_id: endpoint,
                        volume_percent: config.microphone_volume_percent,
                        mixers: if separate_tracks { 0b101 } else { 0b001 },
                        noise_suppression: config.microphone_noise_suppression,
                    },
                )?;
            }

            let max_buffer_seconds = config.max_buffer_seconds();
            let max_size_mb = ((config.video_bitrate_mbps as u64 * max_buffer_seconds as u64)
                .div_ceil(8)
                .saturating_mul(2)
                .saturating_add(64))
            .clamp(128, 8192) as i64;
            let codec = match config.video_codec {
                VideoCodec::H264 => HardwareCodec::H264,
                VideoCodec::H265 => HardwareCodec::HEVC,
            };
            let mut builder = context
                .replay_buffer_builder(
                    "xyra-replay-output",
                    ObsPath::new(&config.clips_directory.to_string_lossy()),
                )
                .max_time_sec(max_buffer_seconds as i64)
                .max_size_mb(max_size_mb)
                .format("xyra-%CCYY%MM%DD-%hh%mm%ss")
                .extension("mp4")
                .allow_spaces(false)
                .video_bitrate(config.video_bitrate_mbps.saturating_mul(1_000))
                .audio_bitrate(192);
            builder = if config.encoder == EncoderBackend::Software {
                builder.x264_encoder(X264Preset::VeryFast)
            } else {
                builder.hardware_encoder(codec, HardwarePreset::Balanced)
            };
            let mut replay = builder
                .build()
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
            let active_encoder = replay
                .get_current_video_encoder()
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?
                .map(|encoder| encoder.id().to_string())
                .map(|id| encoder_backend_from_id(&id))
                .unwrap_or(EncoderBackend::Software);

            if separate_tracks {
                add_audio_encoder(&context, &mut replay, 1, "xyra-desktop-track")?;
                add_audio_encoder(&context, &mut replay, 2, "xyra-microphone-track")?;
            }
            replay
                .start()
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;

            Ok(Self {
                context,
                replay,
                active_encoder,
                max_buffer_seconds,
            })
        }

        pub fn is_running(&self) -> bool {
            self.replay.is_active().unwrap_or(false)
        }

        pub fn active_encoder(&self) -> EncoderBackend {
            self.active_encoder
        }

        pub fn request_replay(
            &self,
            config: &AppConfig,
            requested_seconds: u32,
        ) -> Result<ObsReplayRequest, ObsCaptureError> {
            if !self.is_running() {
                return Err(ObsCaptureError::Replay(
                    "the OBS replay buffer is not running".into(),
                ));
            }
            Ok(ObsReplayRequest {
                _context: self.context.clone(),
                replay: self.replay.clone(),
                ffmpeg_path: config.ffmpeg_path.clone(),
                requested_seconds: requested_seconds.max(1),
                max_buffer_seconds: self.max_buffer_seconds,
            })
        }

        pub fn stop(&mut self) -> Result<(), ObsCaptureError> {
            if self.is_running() {
                self.replay
                    .stop()
                    .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
            }
            Ok(())
        }
    }

    impl Drop for ObsCaptureEngine {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    impl ObsReplayRequest {
        pub fn complete(self) -> Result<Clip, ObsCaptureError> {
            let saved = self
                .replay
                .save_buffer()
                .map_err(|error| ObsCaptureError::Replay(error.to_string()))?
                .into_path_buf();
            let requested = self.requested_seconds.min(self.max_buffer_seconds);
            if requested >= self.max_buffer_seconds || !self.ffmpeg_path.is_file() {
                return Ok(Clip::new(saved, requested as f32));
            }

            let extension = saved
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("mp4");
            let trimmed = saved.with_file_name(format!(
                "xyra-{}-{}s.{extension}",
                Utc::now().format("%Y%m%d-%H%M%S"),
                requested
            ));
            let result = hidden_command(&self.ffmpeg_path)
                .args(["-y", "-sseof", &format!("-{requested}"), "-i"])
                .arg(&saved)
                .args(["-map", "0", "-c", "copy", "-avoid_negative_ts", "make_zero"])
                .arg(&trimmed)
                .output();
            if result.is_ok_and(|output| output.status.success()) && trimmed.is_file() {
                let _ = fs::remove_file(&saved);
                Ok(Clip::new(trimmed, requested as f32))
            } else {
                Ok(Clip::new(saved, self.max_buffer_seconds as f32))
            }
        }
    }

    struct AudioSourceSpec<'a> {
        source_id: &'a str,
        source_name: &'a str,
        endpoint_id: &'a str,
        volume_percent: u32,
        mixers: u32,
        noise_suppression: bool,
    }

    fn obs_output_dimensions(
        config: &AppConfig,
        monitor: &crate::display::MonitorInfo,
    ) -> (u32, u32) {
        let width = config.output_width.max(640) / 2 * 2;
        let height = config.output_height.max(360) / 2 * 2;
        if config.video_aspect_ratio != VideoAspectRatio::Game {
            return (width, height);
        }
        let scale = (width as f64 / monitor.width.max(1) as f64)
            .min(height as f64 / monitor.height.max(1) as f64);
        let game_width = ((monitor.width as f64 * scale).round() as u32).max(2) / 2 * 2;
        let game_height = ((monitor.height as f64 * scale).round() as u32).max(2) / 2 * 2;
        (game_width, game_height)
    }

    fn apply_video_transform(
        item: &impl SceneItemTrait,
        mode: VideoAspectRatio,
        output_width: u32,
        output_height: u32,
    ) -> Result<(), ObsCaptureError> {
        let (bounds_type, crop_to_bounds) = match mode {
            VideoAspectRatio::Stretch16By9 => (ObsBoundsType::Stretch, false),
            VideoAspectRatio::Fit16By9 | VideoAspectRatio::Game => {
                (ObsBoundsType::ScaleInner, false)
            }
            VideoAspectRatio::Crop16By9 => (ObsBoundsType::ScaleOuter, true),
        };
        let transform = ObsTransformInfoBuilder::new()
            .set_bounds(Vec2::new(output_width as f32, output_height as f32))
            .set_bounds_type(bounds_type)
            .set_crop_to_bounds(crop_to_bounds)
            .build(output_width, output_height);
        item.set_transform_info(&transform)
            .map_err(|error| ObsCaptureError::Engine(error.to_string()))
    }

    fn add_audio_source(
        context: &mut ObsContext,
        scene: &mut libobs_wrapper::scenes::ObsSceneRef,
        spec: AudioSourceSpec<'_>,
    ) -> Result<(), ObsCaptureError> {
        let mut settings = context
            .data()
            .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
        settings
            .set_string("device_id", spec.endpoint_id)
            .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
        let item = scene
            .add_and_create_source(SourceInfo::new(
                spec.source_id,
                spec.source_name,
                Some(settings),
                None,
            ))
            .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
        configure_audio_source(
            item.inner_source(),
            spec.volume_percent.min(200) as f32 / 100.0,
            spec.mixers,
        )?;

        if spec.noise_suppression {
            let mut filter_settings = context
                .data()
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
            filter_settings
                .set_string("method", "RNNoise")
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
            let filter = context
                .obs_filter(FilterInfo::new(
                    "noise_suppress_filter_v2",
                    "xyra-microphone-noise-suppression",
                    Some(filter_settings),
                    None,
                ))
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
            use libobs_wrapper::sources::ObsSourceTrait;
            item.inner_source()
                .apply_filter(&filter)
                .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
        }
        Ok(())
    }

    fn configure_audio_source(
        source: &ObsSourceRef,
        volume: f32,
        mixers: u32,
    ) -> Result<(), ObsCaptureError> {
        let source_ptr = source.as_ptr();
        let runtime = source.runtime().clone();
        run_with_obs!(runtime, (source_ptr), move || unsafe {
            libobs::obs_source_set_volume(source_ptr.get_ptr(), volume);
            libobs::obs_source_set_audio_mixers(source_ptr.get_ptr(), mixers);
        })
        .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
        Ok(())
    }

    fn add_audio_encoder(
        context: &ObsContext,
        output: &mut ObsReplayBufferOutputRef,
        mixer_index: usize,
        name: &str,
    ) -> Result<(), ObsCaptureError> {
        let mut settings = context
            .data()
            .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
        settings
            .set_int("bitrate", 192)
            .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
        output
            .create_and_set_audio_encoder(
                AudioEncoderInfo::new(ObsAudioEncoderType::FFMPEG_AAC, name, Some(settings), None),
                mixer_index,
            )
            .map_err(|error| ObsCaptureError::Engine(error.to_string()))?;
        Ok(())
    }

    fn encoder_backend_from_id(id: &str) -> EncoderBackend {
        let id = id.to_ascii_lowercase();
        if id.contains("nvenc") {
            EncoderBackend::Nvidia
        } else if id.contains("amf") {
            EncoderBackend::Amd
        } else if id.contains("qsv") {
            EncoderBackend::Intel
        } else {
            EncoderBackend::Software
        }
    }

    pub fn find_runtime_root() -> Option<PathBuf> {
        let configured = std::env::var_os("XYRA_OBS_RUNTIME").map(PathBuf::from);
        let executable_dir = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let program_files = std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .map(|path| path.join("obs-studio"));
        configured
            .into_iter()
            .chain(executable_dir)
            .chain(program_files)
            .find(|root| runtime_complete(root))
    }

    fn runtime_complete(root: &Path) -> bool {
        let obs_dll = root.join("obs.dll");
        let installed_obs_dll = root.join("bin").join("64bit").join("obs.dll");
        (obs_dll.is_file() || installed_obs_dll.is_file())
            && root.join("data").join("libobs").is_dir()
            && (root.join("xyra-plugins").join("64bit").is_dir()
                || root.join("obs-plugins").join("64bit").is_dir())
    }

    fn configure_dll_search_path(root: &Path) -> Result<(), ObsCaptureError> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::System::LibraryLoader::SetDllDirectoryW;

        let binary_directory = if root.join("obs.dll").is_file() {
            root.to_path_buf()
        } else {
            root.join("bin").join("64bit")
        };
        let wide = binary_directory
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        if unsafe { SetDllDirectoryW(wide.as_ptr()) } == 0 {
            return Err(ObsCaptureError::Engine(format!(
                "Windows could not add the OBS DLL directory {}",
                binary_directory.display()
            )));
        }
        Ok(())
    }

    fn ensure_helper_binaries(root: &Path) -> Result<(), ObsCaptureError> {
        let executable_dir = std::env::current_exe()
            .map_err(|error| ObsCaptureError::Engine(error.to_string()))?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                ObsCaptureError::Engine("Xyra executable has no parent directory".into())
            })?;
        let source_dir = if root.join("obs.dll").is_file() {
            root.to_path_buf()
        } else {
            root.join("bin").join("64bit")
        };
        for helper in [
            "obs-ffmpeg-mux.exe",
            "obs-nvenc-test.exe",
            "obs-amf-test.exe",
            "obs-qsv-test.exe",
        ] {
            let source = source_dir.join(helper);
            let destination = executable_dir.join(helper);
            if destination.is_file() || !source.is_file() {
                continue;
            }
            fs::copy(&source, &destination).map_err(|error| {
                ObsCaptureError::Engine(format!(
                    "could not stage OBS helper {} beside Xyra: {error}",
                    source.display()
                ))
            })?;
        }
        if !executable_dir.join("obs-ffmpeg-mux.exe").is_file() {
            return Err(ObsCaptureError::RuntimeMissing);
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use platform::{ObsCaptureEngine, ObsReplayRequest, find_runtime_root};

#[cfg(not(windows))]
#[derive(Debug)]
pub struct ObsCaptureEngine;

#[cfg(not(windows))]
#[derive(Clone)]
pub struct ObsReplayRequest;

#[cfg(not(windows))]
impl ObsCaptureEngine {
    pub fn start(_config: &AppConfig) -> Result<Self, ObsCaptureError> {
        Err(ObsCaptureError::UnsupportedPlatform)
    }

    pub fn is_running(&self) -> bool {
        false
    }

    pub fn active_encoder(&self) -> EncoderBackend {
        EncoderBackend::Software
    }

    pub fn request_replay(
        &self,
        _config: &AppConfig,
        _requested_seconds: u32,
    ) -> Result<ObsReplayRequest, ObsCaptureError> {
        Err(ObsCaptureError::UnsupportedPlatform)
    }

    pub fn stop(&mut self) -> Result<(), ObsCaptureError> {
        Ok(())
    }
}

#[cfg(not(windows))]
impl ObsReplayRequest {
    pub fn complete(self) -> Result<Clip, ObsCaptureError> {
        Err(ObsCaptureError::UnsupportedPlatform)
    }
}

#[cfg(not(windows))]
pub fn find_runtime_root() -> Option<PathBuf> {
    None
}

pub fn runtime_available() -> bool {
    find_runtime_root().is_some()
}

#[cfg(all(test, windows))]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::CaptureEngine;

    #[test]
    #[ignore = "requires an installed OBS runtime and captures the active display"]
    fn obs_replay_smoke_test() {
        let output = std::env::temp_dir().join(format!("xyra-obs-smoke-{}", uuid::Uuid::new_v4()));
        let mut config = AppConfig {
            capture_engine: CaptureEngine::Obs,
            clip_seconds: 5,
            clips_directory: output.clone(),
            desktop_audio_enabled: true,
            microphone_enabled: true,
            separate_audio_tracks: true,
            ..AppConfig::default()
        };
        for hotkey in &mut config.clip_hotkeys {
            hotkey.enabled = false;
        }

        let mut engine = ObsCaptureEngine::start(&config).expect("OBS replay buffer should start");
        std::thread::sleep(Duration::from_secs(6));
        let clip = engine
            .request_replay(&config, 5)
            .and_then(ObsReplayRequest::complete)
            .expect("OBS replay should save");
        engine.stop().expect("OBS replay buffer should stop");

        assert!(clip.path.is_file());
        assert!(clip.path.metadata().unwrap().len() > 10_000);
    }
}
