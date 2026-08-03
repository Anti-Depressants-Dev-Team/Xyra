use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use ffmpeg_sidecar::download::{
    FfmpegDownloadProgressEvent, download_ffmpeg_package_with_progress, ffmpeg_download_url,
    unpack_ffmpeg_without_extras,
};

#[derive(Debug, Clone, PartialEq)]
pub enum InstallState {
    NotManaged,
    Starting,
    Downloading { downloaded: u64, total: u64 },
    Unpacking,
    Ready,
    Failed(String),
}

impl InstallState {
    pub fn label(&self) -> String {
        match self {
            Self::NotManaged => "Custom FFmpeg path".into(),
            Self::Starting => "Preparing FFmpeg...".into(),
            Self::Downloading { downloaded, total } if *total > 0 => {
                format!(
                    "Downloading FFmpeg {}%",
                    downloaded.saturating_mul(100) / total
                )
            }
            Self::Downloading { .. } => "Downloading FFmpeg...".into(),
            Self::Unpacking => "Installing FFmpeg...".into(),
            Self::Ready => "FFmpeg ready".into(),
            Self::Failed(_) => "FFmpeg setup failed".into(),
        }
    }

    pub fn progress(&self) -> Option<f32> {
        match self {
            Self::Downloading { downloaded, total } if *total > 0 => {
                Some((*downloaded as f32 / *total as f32).clamp(0.0, 1.0))
            }
            Self::Starting => Some(0.0),
            Self::Unpacking => Some(1.0),
            _ => None,
        }
    }
}

pub struct FfmpegInstaller {
    destination: PathBuf,
    state: InstallState,
    receiver: Option<Receiver<InstallState>>,
}

impl FfmpegInstaller {
    pub fn new(destination: PathBuf, managed: bool) -> Self {
        if executable_works(&destination) {
            return Self {
                destination,
                state: InstallState::Ready,
                receiver: None,
            };
        }
        if managed {
            Self::start(destination)
        } else {
            Self {
                destination,
                state: InstallState::NotManaged,
                receiver: None,
            }
        }
    }

    pub fn start(destination: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        let worker_destination = destination.clone();
        thread::spawn(move || {
            let _ = sender.send(InstallState::Starting);
            let result = install(&worker_destination, |state| {
                let _ = sender.send(state);
            });
            match result {
                Ok(()) => {
                    let _ = sender.send(InstallState::Ready);
                }
                Err(error) => {
                    let _ = sender.send(InstallState::Failed(error));
                }
            }
        });
        Self {
            destination,
            state: InstallState::Starting,
            receiver: Some(receiver),
        }
    }

    pub fn poll(&mut self) -> bool {
        let Some(receiver) = &self.receiver else {
            return false;
        };
        let mut changed = false;
        loop {
            match receiver.try_recv() {
                Ok(state) => {
                    self.state = state;
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.receiver = None;
                    break;
                }
            }
        }
        changed
    }

    pub fn state(&self) -> &InstallState {
        &self.state
    }

    pub fn destination(&self) -> &Path {
        &self.destination
    }
}

fn install(destination: &Path, progress: impl Fn(InstallState)) -> Result<(), String> {
    let runtime_dir = destination
        .parent()
        .ok_or_else(|| "managed FFmpeg path has no parent directory".to_owned())?;
    fs::create_dir_all(runtime_dir).map_err(|error| error.to_string())?;

    if destination.exists() && !executable_works(destination) {
        fs::remove_file(destination).map_err(|error| error.to_string())?;
    }

    let url = ffmpeg_download_url().map_err(|error| error.to_string())?;
    let archive = download_ffmpeg_package_with_progress(url, runtime_dir, |event| match event {
        FfmpegDownloadProgressEvent::Downloading {
            total_bytes,
            downloaded_bytes,
        } => progress(InstallState::Downloading {
            downloaded: downloaded_bytes,
            total: total_bytes,
        }),
        FfmpegDownloadProgressEvent::UnpackingArchive => progress(InstallState::Unpacking),
        _ => {}
    })
    .map_err(|error| error.to_string())?;

    progress(InstallState::Unpacking);
    unpack_ffmpeg_without_extras(&archive, runtime_dir).map_err(|error| error.to_string())?;
    if !executable_works(destination) {
        return Err("downloaded FFmpeg did not pass its version check".into());
    }
    Ok(())
}

fn executable_works(path: &Path) -> bool {
    Command::new(path)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::InstallState;

    #[test]
    fn progress_label_has_percentage() {
        let state = InstallState::Downloading {
            downloaded: 25,
            total: 100,
        };
        assert_eq!(state.label(), "Downloading FFmpeg 25%");
        assert_eq!(state.progress(), Some(0.25));
    }
}
