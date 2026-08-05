use std::path::Path;

use crate::{
    process::hidden_command,
    windows_audio::{
        WASAPI_CAPTURE_PREFIX, WASAPI_RENDER_PREFIX, enumerate_capture_devices,
        enumerate_render_devices,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub name: String,
    pub id: String,
}

pub fn enumerate_audio_devices(ffmpeg: &Path) -> std::io::Result<Vec<AudioDevice>> {
    let output = hidden_command(ffmpeg)
        .args([
            "-hide_banner",
            "-list_devices",
            "true",
            "-f",
            "dshow",
            "-i",
            "dummy",
        ])
        .output()?;
    let mut devices = enumerate_render_devices().unwrap_or_default();
    devices.extend(enumerate_capture_devices().unwrap_or_default());
    devices.extend(parse_dshow_devices(&String::from_utf8_lossy(
        &output.stderr,
    )));
    Ok(devices)
}

pub fn recommended_microphone(devices: &[AudioDevice]) -> Option<&AudioDevice> {
    devices
        .iter()
        .find(|device| is_microphone_device(device))
        .or_else(|| {
            devices
                .iter()
                .find(|device| !is_desktop_audio_device(device))
        })
}

pub fn recommended_desktop_audio(devices: &[AudioDevice]) -> Option<&AudioDevice> {
    devices
        .iter()
        .find(|device| is_desktop_audio_device(device))
}

pub fn is_desktop_audio_device(device: &AudioDevice) -> bool {
    if device.id.starts_with(WASAPI_RENDER_PREFIX) {
        return true;
    }
    let name = device.name.to_ascii_lowercase();
    [
        "stereo mix",
        "what u hear",
        "wave out",
        "loopback",
        "sonar - stream",
        "cable output",
        "desktop audio",
    ]
    .iter()
    .any(|hint| name.contains(hint))
}

pub fn is_microphone_device(device: &AudioDevice) -> bool {
    if device.id.starts_with(WASAPI_CAPTURE_PREFIX) {
        return true;
    }
    let name = device.name.to_ascii_lowercase();
    !is_desktop_audio_device(device)
        && (name.contains("microphone")
            || name.contains(" mic")
            || name.starts_with("mic")
            || name.contains("headset"))
}

fn parse_dshow_devices(output: &str) -> Vec<AudioDevice> {
    let mut devices = Vec::new();
    let mut pending_name: Option<String> = None;
    for line in output.lines() {
        if line.contains("(audio)") {
            pending_name = quoted_value(line);
            continue;
        }
        if line.contains("Alternative name")
            && let Some(name) = pending_name.take()
        {
            devices.push(AudioDevice {
                id: quoted_value(line).unwrap_or_else(|| name.clone()),
                name,
            });
        }
    }
    if let Some(name) = pending_name {
        devices.push(AudioDevice {
            id: name.clone(),
            name,
        });
    }
    devices
}

fn quoted_value(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].rfind('"')? + start;
    Some(line[start..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_audio_devices_and_recommends_roles() {
        let listing = r#"
[dshow @ 1] "Microphone (USB Mic)" (audio)
[dshow @ 1]   Alternative name "@device_mic"
[dshow @ 1] "SteelSeries Sonar - Stream" (audio)
[dshow @ 1]   Alternative name "@device_stream"
"#;
        let devices = parse_dshow_devices(listing);
        assert_eq!(devices.len(), 2);
        assert_eq!(recommended_microphone(&devices).unwrap().id, "@device_mic");
        assert_eq!(
            recommended_desktop_audio(&devices).unwrap().id,
            "@device_stream"
        );
        assert!(is_microphone_device(&devices[0]));
        assert!(!is_microphone_device(&devices[1]));
        assert!(is_desktop_audio_device(&devices[1]));
        assert!(is_desktop_audio_device(&AudioDevice {
            name: "Speakers".into(),
            id: format!("{WASAPI_RENDER_PREFIX}endpoint"),
        }));
    }
}
