use std::{
    io::Read,
    num::NonZero,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
};

struct AudioPlayback {
    player: rodio::Player,
    _device: rodio::MixerDeviceSink,
}

impl AudioPlayback {
    fn start(ffmpeg: &Path, path: &Path, position_secs: f32) -> Result<Self, String> {
        let decoded = Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-ss"])
            .arg(format!("{position_secs:.3}"))
            .arg("-i")
            .arg(path)
            .args([
                "-map", "0:a:0?", "-vn", "-ac", "2", "-ar", "48000", "-f", "f32le", "pipe:1",
            ])
            .output()
            .map_err(|error| format!("Could not start FFmpeg audio decoder: {error}"))?;
        if !decoded.status.success() {
            return Err(format!(
                "Could not decode clip audio: {}",
                String::from_utf8_lossy(&decoded.stderr)
                    .lines()
                    .last()
                    .unwrap_or("unknown FFmpeg error")
            ));
        }
        let samples = decoded
            .stdout
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect::<Vec<_>>();
        if samples.is_empty() {
            return Err("This clip has no audio track".into());
        }

        let mut device = rodio::DeviceSinkBuilder::open_default_sink()
            .map_err(|error| format!("Could not open the default audio output: {error}"))?;
        device.log_on_drop(false);
        let player = rodio::Player::connect_new(device.mixer());
        player.pause();
        player.append(rodio::buffer::SamplesBuffer::new(
            NonZero::new(2).unwrap(),
            NonZero::new(48_000).unwrap(),
            samples,
        ));
        Ok(Self {
            player,
            _device: device,
        })
    }

    fn play(&self) {
        self.player.play();
    }

    fn stop(self) {
        self.player.stop();
    }
}

pub const PREVIEW_WIDTH: usize = 960;
pub const PREVIEW_HEIGHT: usize = 540;
const PREVIEW_FPS: f32 = 60.0;

#[derive(Debug)]
pub struct VideoFrame {
    pub rgba: Vec<u8>,
    pub position_secs: f32,
}

enum PlayerEvent {
    Frame(VideoFrame),
    Ended,
    Failed(String),
}

#[derive(Default)]
pub struct VideoPlayer {
    path: Option<PathBuf>,
    duration_secs: f32,
    position_secs: f32,
    playing: bool,
    receiver: Option<Receiver<PlayerEvent>>,
    cancel: Option<Arc<AtomicBool>>,
    error: Option<String>,
    audio: Option<AudioPlayback>,
}

impl VideoPlayer {
    pub fn load(&mut self, path: PathBuf, duration_secs: f32) {
        self.stop_decoder();
        self.path = Some(path);
        self.duration_secs = duration_secs.max(0.0);
        self.position_secs = 0.0;
        self.playing = false;
        self.error = None;
    }

    pub fn request_preview(&mut self, ffmpeg: &Path) {
        self.spawn_decoder(ffmpeg, true);
    }

    pub fn play(&mut self, ffmpeg: &Path) {
        if self.position_secs >= self.duration_secs && self.duration_secs > 0.0 {
            self.position_secs = 0.0;
        }
        let audio = self.prepare_audio(ffmpeg);
        self.spawn_decoder(ffmpeg, false);
        self.install_audio(audio);
    }

    pub fn pause(&mut self) {
        self.stop_decoder();
        self.playing = false;
    }

    pub fn seek(&mut self, ffmpeg: &Path, position_secs: f32) {
        let was_playing = self.playing;
        self.stop_decoder();
        self.position_secs = position_secs.clamp(0.0, self.duration_secs.max(0.0));
        let audio = was_playing.then(|| self.prepare_audio(ffmpeg));
        self.spawn_decoder(ffmpeg, !was_playing);
        if let Some(audio) = audio {
            self.install_audio(audio);
        }
    }

    pub fn poll(&mut self) -> Option<VideoFrame> {
        let Some(receiver) = &self.receiver else {
            return None;
        };
        let mut latest = None;
        while let Ok(event) = receiver.try_recv() {
            match event {
                PlayerEvent::Frame(frame) => {
                    self.position_secs = frame.position_secs.min(self.duration_secs.max(0.0));
                    latest = Some(frame);
                }
                PlayerEvent::Ended => {
                    self.playing = false;
                    if let Some(audio) = self.audio.take() {
                        audio.stop();
                    }
                }
                PlayerEvent::Failed(error) => {
                    self.playing = false;
                    self.error = Some(error);
                    if let Some(audio) = self.audio.take() {
                        audio.stop();
                    }
                }
            }
        }
        latest
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn position_secs(&self) -> f32 {
        self.position_secs
    }

    pub fn duration_secs(&self) -> f32 {
        self.duration_secs
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn spawn_decoder(&mut self, ffmpeg: &Path, single_frame: bool) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.stop_decoder();
        self.error = None;
        self.playing = !single_frame;
        let start = self.position_secs;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::sync_channel(2);
        let ffmpeg = ffmpeg.to_path_buf();
        thread::spawn(move || {
            decode(ffmpeg, path, start, single_frame, worker_cancel, sender);
        });
        self.cancel = Some(cancel);
        self.receiver = Some(receiver);
    }

    fn prepare_audio(&self, ffmpeg: &Path) -> Result<AudioPlayback, String> {
        let Some(path) = self.path.as_deref() else {
            return Err("No clip is loaded".into());
        };
        AudioPlayback::start(ffmpeg, path, self.position_secs)
    }

    fn install_audio(&mut self, audio: Result<AudioPlayback, String>) {
        match audio {
            Ok(audio) => {
                audio.play();
                self.audio = Some(audio);
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn stop_decoder(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.receiver = None;
        if let Some(audio) = self.audio.take() {
            audio.stop();
        }
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.stop_decoder();
    }
}

fn decode(
    ffmpeg: PathBuf,
    input: PathBuf,
    start_secs: f32,
    single_frame: bool,
    cancel: Arc<AtomicBool>,
    sender: SyncSender<PlayerEvent>,
) {
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "error", "-ss"])
        .arg(format!("{start_secs:.3}"));
    if !single_frame {
        command.arg("-re");
    }
    command
        .arg("-i")
        .arg(input)
        .args([
            "-an",
            "-vf",
            "fps=60,scale=960:540:force_original_aspect_ratio=decrease,pad=960:540:(ow-iw)/2:(oh-ih)/2:black",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = sender.try_send(PlayerEvent::Failed(format!(
                "Could not start video decoder: {error}"
            )));
            return;
        }
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = sender.try_send(PlayerEvent::Failed("Video decoder has no output".into()));
        let _ = child.kill();
        return;
    };

    let frame_size = PREVIEW_WIDTH * PREVIEW_HEIGHT * 4;
    let mut frame_index = 0_u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let mut rgba = vec![0_u8; frame_size];
        if let Err(error) = stdout.read_exact(&mut rgba) {
            if error.kind() != std::io::ErrorKind::UnexpectedEof {
                let _ = sender.try_send(PlayerEvent::Failed(format!(
                    "Could not decode video frame: {error}"
                )));
            }
            break;
        }
        let frame = VideoFrame {
            rgba,
            position_secs: start_secs + frame_index as f32 / PREVIEW_FPS,
        };
        frame_index += 1;
        match sender.try_send(PlayerEvent::Frame(frame)) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => break,
        }
        if single_frame {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    if !cancel.load(Ordering::Relaxed) {
        let _ = sender.send(PlayerEvent::Ended);
    }
}

#[cfg(test)]
mod tests {
    use super::VideoPlayer;

    #[test]
    fn loading_resets_timeline() {
        let mut player = VideoPlayer::default();
        player.load("clip.mp4".into(), 30.0);
        assert_eq!(player.position_secs(), 0.0);
        assert_eq!(player.duration_secs(), 30.0);
        assert!(!player.is_playing());
    }
}
