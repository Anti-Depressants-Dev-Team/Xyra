use std::{fs, path::Path, process::Command};

use chrono::{DateTime, Utc};

use crate::model::Clip;

pub fn scan_clips(directory: &Path, ffmpeg_path: Option<&Path>) -> std::io::Result<Vec<Clip>> {
    fs::create_dir_all(directory)?;
    let mut clips = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let is_video = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(value.to_ascii_lowercase().as_str(), "mp4" | "mkv" | "webm")
            });
        if !is_video {
            continue;
        }
        let metadata = entry.metadata()?;
        let created_at = metadata
            .created()
            .or_else(|_| metadata.modified())
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());
        let title = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Untitled clip")
            .to_owned();
        let duration_secs = ffmpeg_path
            .and_then(|ffmpeg| probe_duration(ffmpeg, &path))
            .unwrap_or(0.0);
        let mut clip = Clip::new(path, duration_secs);
        clip.title = title;
        clip.created_at = created_at;
        clips.push(clip);
    }
    clips.sort_by_key(|clip| std::cmp::Reverse(clip.created_at));
    Ok(clips)
}

fn probe_duration(ffmpeg: &Path, video: &Path) -> Option<f32> {
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-i"])
        .arg(video)
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let timestamp = stderr.split("Duration: ").nth(1)?.split(',').next()?;
    parse_duration(timestamp)
}

fn parse_duration(timestamp: &str) -> Option<f32> {
    let mut parts = timestamp.trim().split(':');
    let hours: f32 = parts.next()?.parse().ok()?;
    let minutes: f32 = parts.next()?.parse().ok()?;
    let seconds: f32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn parses_ffmpeg_duration() {
        assert_eq!(parse_duration("00:01:30.50"), Some(90.5));
    }
}
