use std::{
    fs,
    io::Write,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Stdio},
};

use chrono::Utc;
use thiserror::Error;

use crate::{
    config::{AppConfig, CaptureEngine, EncoderBackend, VideoAspectRatio, VideoCodec},
    display::{MonitorInfo, selected_monitor},
    model::Clip,
    obs_capture::{ObsCaptureEngine, ObsReplayRequest},
    process::hidden_command,
    windows_audio::{LoopbackCapture, wasapi_endpoint_id},
};

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error(
        "FFmpeg was not found at '{0}'. Retry the bundled setup or choose a custom path in Settings."
    )]
    FfmpegMissing(String),
    #[error("replay buffer is already running")]
    AlreadyRunning,
    #[error("replay buffer is not running")]
    NotRunning,
    #[error("not enough buffered video yet; wait a few seconds")]
    EmptyBuffer,
    #[error("{0} is not available in the bundled FFmpeg build or graphics driver")]
    EncoderUnavailable(&'static str),
    #[error("OBS capture failed: {0}")]
    Obs(String),
    #[error("media operation failed: {0}")]
    ProcessFailed(String),
    #[error("file operation failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Default)]
pub struct CaptureManager {
    obs: Option<ObsCaptureEngine>,
    child: Option<Child>,
    loopback: Option<LoopbackCapture>,
    active_encoder: Option<EncoderBackend>,
}

pub enum ReplaySaveRequest {
    Obs(ObsReplayRequest),
    Ffmpeg {
        config: AppConfig,
        clip_seconds: u32,
        capture_running: bool,
    },
}

impl ReplaySaveRequest {
    pub fn complete(self) -> Result<Clip, CaptureError> {
        match self {
            Self::Obs(request) => request
                .complete()
                .map_err(|error| CaptureError::Obs(error.to_string())),
            Self::Ffmpeg {
                config,
                clip_seconds,
                capture_running,
            } => CaptureManager::save_replay_snapshot(&config, clip_seconds, capture_running),
        }
    }
}

impl CaptureManager {
    pub fn is_running(&mut self) -> bool {
        if let Some(obs) = self.obs.as_ref() {
            if obs.is_running() {
                return true;
            }
            self.obs = None;
            self.active_encoder = None;
        }
        let finished = self
            .child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
            .is_some();
        if finished {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            if let Some(mut loopback) = self.loopback.take() {
                loopback.stop();
            }
            self.active_encoder = None;
        }
        self.child.is_some()
    }

    pub fn ffmpeg_available(config: &AppConfig) -> bool {
        hidden_command(&config.ffmpeg_path)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    pub fn start(&mut self, config: &AppConfig) -> Result<(), CaptureError> {
        if self.is_running() {
            return Err(CaptureError::AlreadyRunning);
        }
        if matches!(
            config.capture_engine,
            CaptureEngine::Auto | CaptureEngine::Obs
        ) {
            match ObsCaptureEngine::start(config) {
                Ok(obs) => {
                    self.active_encoder = Some(obs.active_encoder());
                    self.obs = Some(obs);
                    return Ok(());
                }
                Err(error) if config.capture_engine == CaptureEngine::Auto => {
                    eprintln!(
                        "OBS engine could not start; using FFmpeg compatibility mode: {error}"
                    );
                }
                Err(error) => return Err(CaptureError::Obs(error.to_string())),
            }
        }
        self.start_ffmpeg(config)
    }

    fn start_ffmpeg(&mut self, config: &AppConfig) -> Result<(), CaptureError> {
        if !Self::ffmpeg_available(config) {
            return Err(CaptureError::FfmpegMissing(
                config.ffmpeg_path.display().to_string(),
            ));
        }
        let mut capture_config = config.clone();
        capture_config.encoder = match config.encoder {
            EncoderBackend::Auto => Self::detect_encoder(&config.ffmpeg_path),
            requested if encoder_available(&config.ffmpeg_path, requested) => requested,
            requested => return Err(CaptureError::EncoderUnavailable(requested.label())),
        };
        fs::create_dir_all(&capture_config.buffer_directory)?;
        clear_segments(&capture_config.buffer_directory)?;
        let output = capture_config.buffer_directory.join("segment-%06d.mp4");
        let capture_log = fs::File::create(capture_config.buffer_directory.join("capture.log"))?;
        let monitor =
            selected_monitor(capture_config.capture_monitor.as_deref()).ok_or_else(|| {
                CaptureError::ProcessFailed("Windows did not report an available display".into())
            })?;
        let loopback_endpoint = capture_config
            .desktop_audio_enabled
            .then_some(capture_config.desktop_audio_device.as_deref())
            .flatten()
            .and_then(wasapi_endpoint_id)
            .map(str::to_owned);
        let listener = if loopback_endpoint.is_some() {
            Some(TcpListener::bind(("127.0.0.1", 0))?)
        } else {
            None
        };
        let loopback_port = listener
            .as_ref()
            .and_then(|listener| listener.local_addr().ok())
            .map(|address| address.port());
        // Capture, encode, mix audio, and segment in one process. The previous
        // NVIDIA path used a second FFmpeg instance and a UDP bridge, which could
        // overrun under GPU contention and was the main source of skipped frames.
        let mut child = hidden_command(&capture_config.ffmpeg_path)
            .args(capture_args_with_loopback(
                &capture_config,
                &monitor,
                &output,
                loopback_port,
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(capture_log))
            .spawn()?;
        if let (Some(endpoint), Some(listener)) = (loopback_endpoint, listener) {
            match LoopbackCapture::start(endpoint, listener) {
                Ok(loopback) => self.loopback = Some(loopback),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CaptureError::ProcessFailed(error));
                }
            }
        }
        self.child = Some(child);
        self.active_encoder = Some(capture_config.encoder);
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), CaptureError> {
        if let Some(mut obs) = self.obs.take() {
            obs.stop()
                .map_err(|error| CaptureError::Obs(error.to_string()))?;
            self.active_encoder = None;
            return Ok(());
        }
        let mut child = self.child.take().ok_or(CaptureError::NotRunning)?;
        request_child_stop(&mut child);
        if let Some(mut loopback) = self.loopback.take() {
            loopback.stop();
        }
        finish_child(child);
        self.active_encoder = None;
        Ok(())
    }

    pub fn active_encoder(&self) -> Option<EncoderBackend> {
        self.active_encoder
    }

    pub fn active_engine(&self) -> Option<CaptureEngine> {
        if self.obs.is_some() {
            Some(CaptureEngine::Obs)
        } else if self.child.is_some() {
            Some(CaptureEngine::Ffmpeg)
        } else {
            None
        }
    }

    pub fn request_replay_save(
        &self,
        config: &AppConfig,
        clip_seconds: u32,
    ) -> Result<ReplaySaveRequest, CaptureError> {
        if let Some(obs) = self.obs.as_ref() {
            return obs
                .request_replay(config, clip_seconds)
                .map(ReplaySaveRequest::Obs)
                .map_err(|error| CaptureError::Obs(error.to_string()));
        }
        Ok(ReplaySaveRequest::Ffmpeg {
            config: config.clone(),
            clip_seconds,
            capture_running: self.child.is_some(),
        })
    }

    pub fn detect_encoder(ffmpeg: &Path) -> EncoderBackend {
        [
            EncoderBackend::Nvidia,
            EncoderBackend::Amd,
            EncoderBackend::Intel,
        ]
        .into_iter()
        .find(|backend| encoder_available(ffmpeg, *backend))
        .unwrap_or(EncoderBackend::Software)
    }

    pub fn save_replay(&self, config: &AppConfig) -> Result<Clip, CaptureError> {
        self.save_replay_duration(config, config.clip_seconds)
    }

    pub fn save_replay_duration(
        &self,
        config: &AppConfig,
        clip_seconds: u32,
    ) -> Result<Clip, CaptureError> {
        Self::save_replay_snapshot(config, clip_seconds, self.child.is_some())
    }

    /// Materializes a replay from complete buffer segments. This function owns
    /// no recorder state, so callers can run it on a worker thread without
    /// blocking the application's event loop.
    pub fn save_replay_snapshot(
        config: &AppConfig,
        clip_seconds: u32,
        capture_running: bool,
    ) -> Result<Clip, CaptureError> {
        let mut segments = list_segments(&config.buffer_directory)?;
        // FFmpeg may still be writing the newest segment. Only concatenate complete files.
        if capture_running && !segments.is_empty() {
            segments.pop();
        }
        let wanted = clip_seconds.max(1).div_ceil(config.segment_seconds) as usize;
        if segments.is_empty() {
            return Err(CaptureError::EmptyBuffer);
        }
        if segments.len() > wanted {
            segments = segments.split_off(segments.len() - wanted);
        }
        fs::create_dir_all(&config.clips_directory)?;
        let output = config
            .clips_directory
            .join(format!("xyra-{}.mp4", Utc::now().format("%Y%m%d-%H%M%S")));
        concatenate(
            &config.ffmpeg_path,
            &segments,
            &output,
            config.segment_seconds,
        )?;
        Ok(Clip::new(
            output,
            (segments.len() as u32 * config.segment_seconds) as f32,
        ))
    }

    pub fn export_trimmed(
        config: &AppConfig,
        input: &Path,
        output: &Path,
        start: f32,
        end: f32,
    ) -> Result<(), CaptureError> {
        let result = hidden_command(&config.ffmpeg_path)
            .args([
                "-y",
                "-ss",
                &format!("{start:.3}"),
                "-to",
                &format!("{end:.3}"),
                "-i",
            ])
            .arg(input)
            .args([
                "-map", "0:v:0", "-map", "0:a?", "-c:v", "libx264", "-preset", "fast", "-crf",
                "18", "-pix_fmt", "yuv420p", "-c:a", "aac",
            ])
            .arg(output)
            .output()
            .map_err(|error| CaptureError::ProcessFailed(error.to_string()))?;
        if !result.status.success() {
            return Err(CaptureError::ProcessFailed(
                String::from_utf8_lossy(&result.stderr)
                    .lines()
                    .last()
                    .unwrap_or("unknown FFmpeg error")
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

impl Drop for CaptureManager {
    fn drop(&mut self) {
        if let Some(mut loopback) = self.loopback.take() {
            loopback.stop();
        }
        if let Some(mut child) = self.child.take() {
            request_child_stop(&mut child);
            finish_child(child);
        }
        self.active_encoder = None;
    }
}

fn request_child_stop(child: &mut Child) {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"q\n");
    }
}

fn finish_child(mut child: Child) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if child.try_wait().is_ok_and(|status| status.is_some()) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn encoder_available(ffmpeg: &Path, backend: EncoderBackend) -> bool {
    let encoder = match backend {
        EncoderBackend::Auto => return true,
        EncoderBackend::Software => "libx264",
        EncoderBackend::Nvidia => "h264_nvenc",
        EncoderBackend::Amd => "h264_amf",
        EncoderBackend::Intel => "h264_qsv",
    };
    hidden_command(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            // Current NVIDIA drivers reject dimensions below the encoder's
            // hardware minimum even for a capability probe.
            "color=size=256x256:rate=1",
            "-frames:v",
            "1",
            "-an",
            "-c:v",
            encoder,
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
fn capture_args(config: &AppConfig, monitor: &MonitorInfo, output: &Path) -> Vec<String> {
    capture_args_with_loopback(config, monitor, output, None)
}

fn capture_args_with_loopback(
    config: &AppConfig,
    monitor: &MonitorInfo,
    output: &Path,
    loopback_port: Option<u16>,
) -> Vec<String> {
    let output_width = config.output_width.max(640) / 2 * 2;
    let output_height = config.output_height.max(360) / 2 * 2;
    let use_ddagrab = uses_ddagrab(config, monitor);
    let codec = match (config.encoder, config.video_codec) {
        (EncoderBackend::Auto, VideoCodec::H264) => "libx264",
        (EncoderBackend::Auto, VideoCodec::H265) => "libx265",
        (EncoderBackend::Software, VideoCodec::H264) => "libx264",
        (EncoderBackend::Software, VideoCodec::H265) => "libx265",
        (EncoderBackend::Nvidia, VideoCodec::H264) => "h264_nvenc",
        (EncoderBackend::Nvidia, VideoCodec::H265) => "hevc_nvenc",
        (EncoderBackend::Amd, VideoCodec::H264) => "h264_amf",
        (EncoderBackend::Amd, VideoCodec::H265) => "hevc_amf",
        (EncoderBackend::Intel, VideoCodec::H264) => "h264_qsv",
        (EncoderBackend::Intel, VideoCodec::H265) => "hevc_qsv",
    };
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "warning".into(),
        "-stats_period".into(),
        "30".into(),
        "-stats".into(),
        "-y".into(),
    ];
    if use_ddagrab {
        args.extend([
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!(
                "ddagrab=output_idx={}:framerate={}:draw_mouse={}:dup_frames=1",
                monitor.output_index,
                config.frame_rate,
                u8::from(config.capture_cursor)
            ),
        ]);
    } else {
        args.extend([
            "-thread_queue_size".into(),
            "1024".into(),
            "-f".into(),
            "gdigrab".into(),
            "-framerate".into(),
            config.frame_rate.to_string(),
            "-draw_mouse".into(),
            u8::from(config.capture_cursor).to_string(),
            "-offset_x".into(),
            monitor.x.to_string(),
            "-offset_y".into(),
            monitor.y.to_string(),
            "-video_size".into(),
            format!("{}x{}", monitor.width, monitor.height),
            "-i".into(),
            "desktop".into(),
        ]);
    }
    let audio_sources = audio_sources(config);
    for source in &audio_sources {
        append_audio_input_args(&mut args, source, loopback_port);
    }
    if use_ddagrab {
        let direct_size = matches!(
            config.video_aspect_ratio,
            VideoAspectRatio::Stretch16By9 | VideoAspectRatio::Game
        ) && output_width == monitor.width
            && output_height == monitor.height;
        if !direct_size {
            // Desktop Duplication is still dramatically faster than GDI on
            // high-resolution monitors. Download only when a transform is
            // required; same-size capture remains zero-copy into NVENC.
            args.extend([
                "-vf".into(),
                dda_software_filter(
                    config.video_aspect_ratio,
                    monitor,
                    output_width,
                    output_height,
                ),
            ]);
        }
    } else {
        args.extend([
            "-vf".into(),
            video_filter(
                config.video_aspect_ratio,
                monitor,
                output_width,
                output_height,
            ),
        ]);
    }
    args.extend(["-c:v".into(), codec.into()]);
    if !use_ddagrab {
        args.extend(["-pix_fmt".into(), "yuv420p".into()]);
    }
    match config.encoder {
        EncoderBackend::Auto | EncoderBackend::Software => {
            args.extend(["-preset".into(), "veryfast".into()])
        }
        EncoderBackend::Nvidia => args.extend([
            "-preset".into(),
            "p2".into(),
            "-tune".into(),
            "ll".into(),
            "-rc".into(),
            "cbr".into(),
            "-multipass".into(),
            "disabled".into(),
            "-rc-lookahead".into(),
            "0".into(),
            "-bf".into(),
            "0".into(),
            "-spatial-aq".into(),
            "0".into(),
            "-temporal-aq".into(),
            "0".into(),
            "-forced-idr".into(),
            "1".into(),
        ]),
        EncoderBackend::Amd => args.extend(["-quality".into(), "speed".into()]),
        EncoderBackend::Intel => args.extend(["-preset".into(), "veryfast".into()]),
    }
    append_audio_output_args(&mut args, config, &audio_sources);
    args.extend([
        "-b:v".into(),
        format!("{}M", config.video_bitrate_mbps),
        "-maxrate".into(),
        format!("{}M", config.video_bitrate_mbps),
        "-bufsize".into(),
        format!("{}M", config.video_bitrate_mbps.saturating_mul(2)),
        "-g".into(),
        config
            .frame_rate
            .saturating_mul(config.segment_seconds)
            .to_string(),
        "-fps_mode".into(),
        "cfr".into(),
        "-force_key_frames".into(),
        format!("expr:gte(t,n_forced*{})", config.segment_seconds),
        "-f".into(),
        "segment".into(),
        "-segment_time".into(),
        config.segment_seconds.to_string(),
        "-segment_wrap".into(),
        (config.max_buffer_seconds().div_ceil(config.segment_seconds) + 3).to_string(),
        "-reset_timestamps".into(),
        "1".into(),
        output.display().to_string(),
    ]);
    args
}

fn uses_ddagrab(config: &AppConfig, _monitor: &MonitorInfo) -> bool {
    cfg!(windows)
        && config.encoder == EncoderBackend::Nvidia
        && !(config.capture_cursor && config.animated_cursor_compatibility)
}

fn dda_software_filter(
    mode: VideoAspectRatio,
    monitor: &MonitorInfo,
    output_width: u32,
    output_height: u32,
) -> String {
    let transform = match mode {
        VideoAspectRatio::Stretch16By9 => {
            format!("scale={output_width}:{output_height}:flags=fast_bilinear,setsar=1")
        }
        VideoAspectRatio::Fit16By9 => format!(
            "scale={output_width}:{output_height}:force_original_aspect_ratio=decrease:flags=fast_bilinear,pad={output_width}:{output_height}:(ow-iw)/2:(oh-ih)/2:black,setsar=1"
        ),
        VideoAspectRatio::Game => {
            let (width, height) =
                fit_inside(monitor.width, monitor.height, output_width, output_height);
            format!("scale={width}:{height}:flags=fast_bilinear,setsar=1")
        }
        VideoAspectRatio::Crop16By9 => format!(
            "scale={output_width}:{output_height}:force_original_aspect_ratio=increase:flags=fast_bilinear,crop={output_width}:{output_height},setsar=1"
        ),
    };
    format!("hwdownload,format=bgra,{transform},format=nv12")
}

fn append_audio_input_args(
    args: &mut Vec<String>,
    source: &AudioSource<'_>,
    loopback_port: Option<u16>,
) {
    if matches!(source.kind, AudioSourceKind::Desktop)
        && wasapi_endpoint_id(source.device).is_some()
    {
        args.extend([
            "-thread_queue_size".into(),
            "1024".into(),
            "-f".into(),
            "f32le".into(),
            "-ar".into(),
            "48000".into(),
            "-ac".into(),
            "2".into(),
            "-i".into(),
            format!("tcp://127.0.0.1:{}", loopback_port.unwrap_or_default()),
        ]);
    } else {
        args.extend([
            "-thread_queue_size".into(),
            "1024".into(),
            "-use_wallclock_as_timestamps".into(),
            "1".into(),
            "-f".into(),
            "dshow".into(),
            "-audio_buffer_size".into(),
            "50".into(),
            "-i".into(),
            format!("audio={}", source.device),
        ]);
    }
}

#[derive(Clone, Copy)]
enum AudioSourceKind {
    Desktop,
    Microphone,
}

struct AudioSource<'a> {
    device: &'a str,
    volume_percent: u32,
    kind: AudioSourceKind,
}

fn audio_sources(config: &AppConfig) -> Vec<AudioSource<'_>> {
    let mut sources = Vec::new();
    if config.desktop_audio_enabled
        && let Some(device) = config.desktop_audio_device.as_deref()
    {
        sources.push(AudioSource {
            device,
            volume_percent: config.desktop_audio_volume_percent,
            kind: AudioSourceKind::Desktop,
        });
    }
    if config.microphone_enabled
        && let Some(device) = config.microphone_device.as_deref()
    {
        sources.push(AudioSource {
            device,
            volume_percent: config.microphone_volume_percent,
            kind: AudioSourceKind::Microphone,
        });
    }
    sources
}

fn append_audio_output_args(
    args: &mut Vec<String>,
    config: &AppConfig,
    sources: &[AudioSource<'_>],
) {
    if sources.is_empty() {
        args.push("-an".into());
        return;
    }

    let separate_with_stems = config.separate_audio_tracks && sources.len() > 1;
    let mut chains = Vec::new();
    for (source_index, source) in sources.iter().enumerate() {
        let input_index = source_index + 1;
        let output_label = format!("audio{source_index}");
        let mut filters = vec!["aresample=48000:async=1:first_pts=0".to_owned()];
        if matches!(source.kind, AudioSourceKind::Microphone) {
            if config.microphone_noise_suppression {
                filters.push("afftdn=nf=-25".into());
            }
            if config.microphone_mono {
                filters.push("pan=stereo|c0=c0|c1=c0".into());
            }
        }
        filters.push(format!(
            "volume={:.2}",
            source.volume_percent.min(200) as f32 / 100.0
        ));
        chains.push(if separate_with_stems {
            format!(
                "[{input_index}:a]{},asplit=2[{output_label}_mix][{output_label}]",
                filters.join(",")
            )
        } else {
            format!("[{input_index}:a]{}[{output_label}]", filters.join(","))
        });
    }

    args.extend(["-filter_complex".into(), {
        if sources.len() > 1 {
            let inputs = (0..sources.len())
                .map(|index| {
                    if separate_with_stems {
                        format!("[audio{index}_mix]")
                    } else {
                        format!("[audio{index}]")
                    }
                })
                .collect::<String>();
            chains.push(format!(
                "{inputs}amix=inputs={}:duration=longest:dropout_transition=0:normalize=0[audio_mix]",
                sources.len()
            ));
        }
        chains.join(";")
    }]);
    args.extend(["-map".into(), "0:v:0".into()]);

    if separate_with_stems {
        args.extend([
            "-map".into(),
            "[audio_mix]".into(),
            "-metadata:s:a:0".into(),
            "title=Game + Microphone Mix".into(),
            "-disposition:a:0".into(),
            "default".into(),
        ]);
        for (index, source) in sources.iter().enumerate() {
            args.extend(["-map".into(), format!("[audio{index}]")]);
            args.extend([
                format!("-metadata:s:a:{}", index + 1),
                format!(
                    "title={}",
                    match source.kind {
                        AudioSourceKind::Desktop => "Desktop Audio",
                        AudioSourceKind::Microphone => "Microphone",
                    }
                ),
                format!("-disposition:a:{}", index + 1),
                "0".into(),
            ]);
        }
    } else {
        args.extend([
            "-map".into(),
            if sources.len() == 1 {
                "[audio0]".into()
            } else {
                "[audio_mix]".into()
            },
        ]);
    }
    args.extend([
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-ar".into(),
        "48000".into(),
    ]);
}

fn video_filter(
    mode: VideoAspectRatio,
    monitor: &MonitorInfo,
    output_width: u32,
    output_height: u32,
) -> String {
    match mode {
        VideoAspectRatio::Stretch16By9 => {
            format!("scale={output_width}:{output_height},setsar=1")
        }
        VideoAspectRatio::Fit16By9 => format!(
            "scale={output_width}:{output_height}:force_original_aspect_ratio=decrease,pad={output_width}:{output_height}:(ow-iw)/2:(oh-ih)/2:black,setsar=1"
        ),
        VideoAspectRatio::Game => {
            let (width, height) =
                fit_inside(monitor.width, monitor.height, output_width, output_height);
            format!("scale={width}:{height},setsar=1")
        }
        VideoAspectRatio::Crop16By9 => format!(
            "scale={output_width}:{output_height}:force_original_aspect_ratio=increase,crop={output_width}:{output_height},setsar=1"
        ),
    }
}

fn fit_inside(
    source_width: u32,
    source_height: u32,
    max_width: u32,
    max_height: u32,
) -> (u32, u32) {
    let (width, height) = if u64::from(source_width) * u64::from(max_height)
        > u64::from(source_height) * u64::from(max_width)
    {
        (
            max_width,
            ((u64::from(max_width) * u64::from(source_height) + u64::from(source_width) / 2)
                / u64::from(source_width)) as u32,
        )
    } else {
        (
            ((u64::from(max_height) * u64::from(source_width) + u64::from(source_height) / 2)
                / u64::from(source_height)) as u32,
            max_height,
        )
    };
    (width.max(2) / 2 * 2, height.max(2) / 2 * 2)
}

fn list_segments(directory: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut entries: Vec<_> = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "mp4"))
        .collect();
    entries.sort_by_key(|path| fs::metadata(path).and_then(|m| m.modified()).ok());
    Ok(entries)
}

fn clear_segments(directory: &Path) -> std::io::Result<()> {
    for path in list_segments(directory)? {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn concatenate(
    ffmpeg: &Path,
    segments: &[PathBuf],
    output: &Path,
    segment_seconds: u32,
) -> Result<(), CaptureError> {
    let list_path = output.with_extension("concat.txt");
    let list = concat_list(segments, segment_seconds);
    fs::write(&list_path, list)?;
    let result = hidden_command(ffmpeg)
        .args(["-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list_path)
        .args(["-map", "0", "-c", "copy"])
        .arg(output)
        .output();
    let _ = fs::remove_file(&list_path);
    let result = result.map_err(|error| CaptureError::ProcessFailed(error.to_string()))?;
    if !result.status.success() {
        return Err(CaptureError::ProcessFailed(
            String::from_utf8_lossy(&result.stderr)
                .lines()
                .last()
                .unwrap_or("unknown FFmpeg error")
                .to_owned(),
        ));
    }
    Ok(())
}

fn concat_list(segments: &[PathBuf], segment_seconds: u32) -> String {
    segments
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let file = format!(
                "file '{}'",
                path.display()
                    .to_string()
                    .replace('\\', "/")
                    .replace('\'', "'\\''")
            );
            if index + 1 < segments.len() {
                format!("{file}\nduration {segment_seconds}.000000")
            } else {
                file
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_uses_wrapped_two_second_segments() {
        let config = AppConfig::default();
        let monitor = MonitorInfo {
            output_index: 1,
            id: "display2".into(),
            label: "Display 2".into(),
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
            primary: false,
        };
        let args = capture_args(&config, &monitor, Path::new("buffer.mp4"));
        let joined = args.join(" ");
        assert!(joined.contains("-segment_time 2"));
        assert!(joined.contains(&format!(
            "-segment_wrap {}",
            config.max_buffer_seconds().div_ceil(config.segment_seconds) + 3
        )));
        assert!(joined.contains("gdigrab"));
        assert!(joined.contains("-offset_x -1920"));
        assert!(joined.contains("-video_size 1920x1080"));
        assert!(joined.contains("-c:v libx264"));
    }

    #[test]
    fn concat_list_uses_the_exact_capture_clock() {
        let list = concat_list(
            &[
                PathBuf::from("segment-1.mp4"),
                PathBuf::from("segment-2.mp4"),
            ],
            2,
        );
        assert_eq!(list.matches("duration 2.000000").count(), 1);
    }

    #[test]
    fn capture_uses_selected_hardware_encoder_and_codec() {
        let config = AppConfig {
            encoder: EncoderBackend::Nvidia,
            video_codec: VideoCodec::H265,
            output_width: 2560,
            output_height: 1440,
            video_bitrate_mbps: 24,
            animated_cursor_compatibility: false,
            ..AppConfig::default()
        };
        let monitor = MonitorInfo {
            output_index: 0,
            id: "display1".into(),
            label: "Display 1".into(),
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
            primary: true,
        };
        let joined = capture_args(&config, &monitor, Path::new("buffer.mp4")).join(" ");
        assert!(joined.contains("-c:v hevc_nvenc"));
        assert!(joined.contains("-preset p2 -tune ll -rc cbr -multipass disabled"));
        assert!(joined.contains("-rc-lookahead 0 -bf 0 -spatial-aq 0 -temporal-aq 0"));
        assert!(joined.contains("-b:v 24M"));
        if cfg!(windows) {
            assert!(joined.contains("-f lavfi -i ddagrab=output_idx=0:framerate=60"));
            assert!(!joined.contains("-pix_fmt"));
            assert!(!joined.contains("-vf"));
        } else {
            assert!(joined.contains("gdigrab"));
        }
    }

    #[test]
    fn nvidia_downscale_uses_desktop_duplication_instead_of_gdi() {
        let monitor = MonitorInfo {
            output_index: 0,
            id: "display1".into(),
            label: "Display 1".into(),
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
            primary: true,
        };
        let fast = AppConfig {
            encoder: EncoderBackend::Nvidia,
            output_width: 1920,
            output_height: 1080,
            animated_cursor_compatibility: false,
            ..AppConfig::default()
        };
        let joined = capture_args(&fast, &monitor, Path::new("buffer.mp4")).join(" ");
        if cfg!(windows) {
            assert!(joined.contains("ddagrab=output_idx=0"));
            assert!(joined.contains("hwdownload,format=bgra,scale=1920:1080:flags=fast_bilinear"));
            assert!(!joined.contains("gdigrab"));
        }

        let compatible = AppConfig {
            animated_cursor_compatibility: true,
            ..fast
        };
        let joined = capture_args(&compatible, &monitor, Path::new("buffer.mp4")).join(" ");
        assert!(joined.contains("gdigrab"));
        assert!(joined.contains("-draw_mouse 1"));
    }

    #[test]
    fn aspect_ratio_modes_build_distinct_video_filters() {
        let monitor = MonitorInfo {
            output_index: 0,
            id: "ultrawide".into(),
            label: "Ultrawide".into(),
            x: 0,
            y: 0,
            width: 3440,
            height: 1440,
            primary: true,
        };
        assert_eq!(
            video_filter(VideoAspectRatio::Stretch16By9, &monitor, 1920, 1080),
            "scale=1920:1080,setsar=1"
        );
        assert!(
            video_filter(VideoAspectRatio::Fit16By9, &monitor, 1920, 1080)
                .contains("force_original_aspect_ratio=decrease,pad=1920:1080")
        );
        assert_eq!(
            video_filter(VideoAspectRatio::Game, &monitor, 1920, 1080),
            "scale=1920:804,setsar=1"
        );
        assert!(
            video_filter(VideoAspectRatio::Crop16By9, &monitor, 1920, 1080)
                .contains("force_original_aspect_ratio=increase,crop=1920:1080")
        );
    }

    #[test]
    fn capture_mixes_desktop_and_filtered_microphone_audio() {
        let config = AppConfig {
            desktop_audio_enabled: true,
            desktop_audio_device: Some("desktop-device".into()),
            desktop_audio_volume_percent: 100,
            microphone_enabled: true,
            microphone_device: Some("microphone-device".into()),
            microphone_volume_percent: 60,
            separate_audio_tracks: false,
            microphone_noise_suppression: true,
            microphone_mono: true,
            ..AppConfig::default()
        };
        let monitor = MonitorInfo {
            output_index: 0,
            id: "display1".into(),
            label: "Display 1".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            primary: true,
        };
        let joined = capture_args(&config, &monitor, Path::new("buffer.mp4")).join(" ");
        assert!(joined.contains("-f dshow -audio_buffer_size 50 -i audio=desktop-device"));
        assert!(joined.contains("-i audio=microphone-device"));
        assert!(joined.contains("afftdn=nf=-25,pan=stereo|c0=c0|c1=c0,volume=0.60"));
        assert!(joined.contains("amix=inputs=2"));
        assert!(joined.contains("-map [audio_mix] -c:a aac -b:a 192k"));
    }

    #[test]
    fn capture_accepts_native_wasapi_playback_audio() {
        let config = AppConfig {
            desktop_audio_enabled: true,
            desktop_audio_device: Some("wasapi-render:windows-endpoint".into()),
            microphone_enabled: false,
            ..AppConfig::default()
        };
        let monitor = MonitorInfo {
            output_index: 0,
            id: "display1".into(),
            label: "Display 1".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            primary: true,
        };
        let joined =
            capture_args_with_loopback(&config, &monitor, Path::new("buffer.mp4"), Some(43123))
                .join(" ");
        assert!(joined.contains("-f f32le -ar 48000 -ac 2"));
        assert!(joined.contains("tcp://127.0.0.1:43123"));
        assert!(!joined.contains("audio=wasapi-render"));
    }

    #[test]
    fn separate_audio_tracks_are_named_and_mapped() {
        let config = AppConfig {
            desktop_audio_enabled: true,
            desktop_audio_device: Some("desktop-device".into()),
            microphone_enabled: true,
            microphone_device: Some("microphone-device".into()),
            separate_audio_tracks: true,
            ..AppConfig::default()
        };
        let monitor = MonitorInfo {
            output_index: 0,
            id: "display1".into(),
            label: "Display 1".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            primary: true,
        };
        let joined = capture_args(&config, &monitor, Path::new("buffer.mp4")).join(" ");
        assert!(joined.contains("asplit=2[audio0_mix][audio0]"));
        assert!(joined.contains(
            "-map [audio_mix] -metadata:s:a:0 title=Game + Microphone Mix -disposition:a:0 default"
        ));
        assert!(joined.contains("-map [audio0] -metadata:s:a:1 title=Desktop Audio"));
        assert!(joined.contains("-map [audio1] -metadata:s:a:2 title=Microphone"));
        assert!(joined.contains("amix=inputs=2"));
    }
}
