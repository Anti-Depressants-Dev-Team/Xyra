use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use chrono::Utc;
use thiserror::Error;

use crate::{
    config::{AppConfig, EncoderBackend, VideoAspectRatio, VideoCodec},
    display::{MonitorInfo, selected_monitor},
    model::Clip,
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
    #[error("media operation failed: {0}")]
    ProcessFailed(String),
    #[error("file operation failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Default)]
pub struct CaptureManager {
    child: Option<Child>,
}

impl CaptureManager {
    pub fn is_running(&mut self) -> bool {
        let finished = self
            .child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
            .is_some();
        if finished {
            self.child = None;
        }
        self.child.is_some()
    }

    pub fn ffmpeg_available(config: &AppConfig) -> bool {
        Command::new(&config.ffmpeg_path)
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
        if !Self::ffmpeg_available(config) {
            return Err(CaptureError::FfmpegMissing(
                config.ffmpeg_path.display().to_string(),
            ));
        }
        fs::create_dir_all(&config.buffer_directory)?;
        clear_segments(&config.buffer_directory)?;
        let output = config.buffer_directory.join("segment-%06d.mp4");
        let capture_log = fs::File::create(config.buffer_directory.join("capture.log"))?;
        let monitor = selected_monitor(config.capture_monitor.as_deref()).ok_or_else(|| {
            CaptureError::ProcessFailed("Windows did not report an available display".into())
        })?;
        let child = Command::new(&config.ffmpeg_path)
            .args(capture_args(config, &monitor, &output))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::from(capture_log))
            .spawn()?;
        self.child = Some(child);
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), CaptureError> {
        let mut child = self.child.take().ok_or(CaptureError::NotRunning)?;
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(b"q\n");
        }
        if child.wait().is_err() {
            let _ = child.kill();
        }
        Ok(())
    }

    pub fn save_replay(&self, config: &AppConfig) -> Result<Clip, CaptureError> {
        let mut segments = list_segments(&config.buffer_directory)?;
        // FFmpeg may still be writing the newest segment. Only concatenate complete files.
        if self.child.is_some() && !segments.is_empty() {
            segments.pop();
        }
        let wanted = config.clip_seconds.div_ceil(config.segment_seconds) as usize;
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
        concatenate(&config.ffmpeg_path, &segments, &output)?;
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
        let result = Command::new(&config.ffmpeg_path)
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
        if let Some(child) = self.child.as_mut()
            && let Some(stdin) = child.stdin.as_mut()
        {
            let _ = stdin.write_all(b"q\n");
        }
    }
}

fn capture_args(config: &AppConfig, monitor: &MonitorInfo, output: &Path) -> Vec<String> {
    let output_width = config.output_width.max(640) / 2 * 2;
    let output_height = config.output_height.max(360) / 2 * 2;
    let use_ddagrab = cfg!(windows)
        && config.encoder == EncoderBackend::Nvidia
        && output_width == monitor.width
        && output_height == monitor.height
        && matches!(
            config.video_aspect_ratio,
            VideoAspectRatio::Stretch16By9 | VideoAspectRatio::Game
        );
    let codec = match (config.encoder, config.video_codec) {
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
        "-y".into(),
    ];
    if use_ddagrab {
        args.extend([
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!(
                "ddagrab=output_idx={}:framerate={}:draw_mouse=1:dup_frames=1",
                monitor.output_index, config.frame_rate
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
    if !use_ddagrab {
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
        EncoderBackend::Software => args.extend(["-preset".into(), "veryfast".into()]),
        EncoderBackend::Nvidia => args.extend([
            "-preset".into(),
            "p4".into(),
            "-tune".into(),
            "ll".into(),
            "-rc".into(),
            "cbr".into(),
            "-bf".into(),
            "0".into(),
            "-forced-idr".into(),
            "1".into(),
        ]),
        EncoderBackend::Amd => args.extend(["-quality".into(), "balanced".into()]),
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
        (config.clip_seconds.div_ceil(config.segment_seconds) + 3).to_string(),
        "-reset_timestamps".into(),
        "1".into(),
        output.display().to_string(),
    ]);
    args
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

fn concatenate(ffmpeg: &Path, segments: &[PathBuf], output: &Path) -> Result<(), CaptureError> {
    let list_path = output.with_extension("concat.txt");
    let list = segments
        .iter()
        .map(|path| {
            format!(
                "file '{}'",
                path.display()
                    .to_string()
                    .replace('\\', "/")
                    .replace('\'', "'\\''")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&list_path, list)?;
    let result = Command::new(ffmpeg)
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
        assert!(joined.contains("-segment_wrap 18"));
        assert!(joined.contains("gdigrab"));
        assert!(joined.contains("-offset_x -1920"));
        assert!(joined.contains("-video_size 1920x1080"));
        assert!(joined.contains("-c:v libx264"));
    }

    #[test]
    fn capture_uses_selected_hardware_encoder_and_codec() {
        let config = AppConfig {
            encoder: EncoderBackend::Nvidia,
            video_codec: VideoCodec::H265,
            output_width: 2560,
            output_height: 1440,
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
        assert!(joined.contains("-preset p4 -tune ll -rc cbr -bf 0"));
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
