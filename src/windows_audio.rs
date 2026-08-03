use std::{io, net::TcpListener};

use crate::audio::AudioDevice;

pub const WASAPI_RENDER_PREFIX: &str = "wasapi-render:";

pub fn is_wasapi_render_id(id: &str) -> bool {
    id.starts_with(WASAPI_RENDER_PREFIX)
}

pub fn wasapi_endpoint_id(id: &str) -> Option<&str> {
    id.strip_prefix(WASAPI_RENDER_PREFIX)
}

#[cfg(windows)]
pub fn enumerate_render_devices() -> io::Result<Vec<AudioDevice>> {
    std::thread::spawn(enumerate_render_devices_inner)
        .join()
        .map_err(|_| io::Error::other("WASAPI device scan thread panicked"))?
        .map_err(io::Error::other)
}

#[cfg(not(windows))]
pub fn enumerate_render_devices() -> io::Result<Vec<AudioDevice>> {
    Ok(Vec::new())
}

#[cfg(windows)]
fn enumerate_render_devices_inner() -> Result<Vec<AudioDevice>, String> {
    use wasapi::{DeviceEnumerator, Direction, initialize_mta};

    initialize_mta().ok().map_err(|error| error.to_string())?;
    let enumerator = DeviceEnumerator::new().map_err(|error| error.to_string())?;
    let default_id = enumerator
        .get_default_device(&Direction::Render)
        .and_then(|device| device.get_id())
        .ok();
    let collection = enumerator
        .get_device_collection(&Direction::Render)
        .map_err(|error| error.to_string())?;
    let mut devices = collection
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|device| {
            let id = device.get_id().ok()?;
            let mut name = device.get_friendlyname().ok()?;
            if default_id.as_deref() == Some(id.as_str()) {
                name.push_str("  ·  Default Windows output");
            }
            Some(AudioDevice {
                name,
                id: format!("{WASAPI_RENDER_PREFIX}{id}"),
            })
        })
        .collect::<Vec<_>>();
    devices.sort_by_key(|device| {
        if default_id.as_ref().is_some_and(|default| {
            device.id.strip_prefix(WASAPI_RENDER_PREFIX) == Some(default.as_str())
        }) {
            0
        } else {
            1
        }
    });
    Ok(devices)
}

#[cfg(windows)]
pub struct LoopbackCapture {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(windows))]
pub struct LoopbackCapture;

#[cfg(windows)]
impl LoopbackCapture {
    pub fn start(endpoint_id: String, listener: TcpListener) -> Result<Self, String> {
        use std::sync::{Arc, atomic::AtomicBool, mpsc};
        use std::time::Duration;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("xyra-wasapi-loopback".into())
            .spawn(move || run_loopback(endpoint_id, listener, thread_stop, ready_tx))
            .map_err(|error| error.to_string())?;

        match ready_rx.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                let _ = thread.join();
                Err(error)
            }
            Err(error) => {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                let _ = thread.join();
                Err(format!("WASAPI loopback did not start: {error}"))
            }
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(not(windows))]
impl LoopbackCapture {
    pub fn start(_endpoint_id: String, _listener: TcpListener) -> Result<Self, String> {
        Err("WASAPI loopback is only available on Windows".into())
    }

    pub fn stop(&mut self) {}
}

#[cfg(windows)]
fn run_loopback(
    endpoint_id: String,
    listener: TcpListener,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ready: std::sync::mpsc::SyncSender<Result<(), String>>,
) {
    if let Err(error) = run_loopback_inner(endpoint_id, listener, stop, &ready) {
        let _ = ready.send(Err(error));
    }
}

#[cfg(windows)]
fn run_loopback_inner(
    endpoint_id: String,
    listener: TcpListener,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ready: &std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    use std::{
        collections::VecDeque,
        io::Write,
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };
    use wasapi::{DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta};

    initialize_mta()
        .ok()
        .map_err(|error| format!("Could not initialize WASAPI: {error}"))?;
    let enumerator = DeviceEnumerator::new().map_err(|error| error.to_string())?;
    let collection = enumerator
        .get_device_collection(&Direction::Render)
        .map_err(|error| error.to_string())?;
    let device = collection
        .into_iter()
        .filter_map(Result::ok)
        .find(|device| device.get_id().ok().as_deref() == Some(endpoint_id.as_str()))
        .ok_or_else(|| "The selected Windows playback device is no longer available".to_owned())?;
    let mut audio_client = device
        .get_iaudioclient()
        .map_err(|error| error.to_string())?;
    let desired_format = WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, None);
    let (_, minimum_period) = audio_client
        .get_device_period()
        .map_err(|error| error.to_string())?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: minimum_period,
    };
    audio_client
        .initialize_client(&desired_format, &Direction::Capture, &mode)
        .map_err(|error| format!("Could not open Windows loopback capture: {error}"))?;
    let event = audio_client
        .set_get_eventhandle()
        .map_err(|error| error.to_string())?;
    let capture_client = audio_client
        .get_audiocaptureclient()
        .map_err(|error| error.to_string())?;

    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(7);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err("FFmpeg did not connect to the WASAPI audio relay".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.to_string()),
        }
    };
    stream
        .set_nodelay(true)
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_millis(100)))
        .map_err(|error| error.to_string())?;
    audio_client
        .start_stream()
        .map_err(|error| error.to_string())?;
    let _ = ready.send(Ok(()));

    let mut queue = VecDeque::with_capacity(48_000 * 2 * 4);
    while !stop.load(Ordering::Relaxed) {
        if event.wait_for_event(100).is_err() {
            continue;
        }
        let previous_len = queue.len();
        let info = capture_client
            .read_from_device_to_deque(&mut queue)
            .map_err(|error| error.to_string())?;
        if info.flags.silent {
            for byte in queue.iter_mut().skip(previous_len) {
                *byte = 0;
            }
        }
        while !queue.is_empty() {
            let (front, _) = queue.as_slices();
            match stream.write(front) {
                Ok(0) => return Ok(()),
                Ok(written) => {
                    queue.drain(..written);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::BrokenPipe
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::ConnectionReset
                    ) =>
                {
                    return Ok(());
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    let _ = audio_client.stop_stream();
    Ok(())
}
