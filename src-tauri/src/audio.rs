//! Final WebAudio PCM -> native output. No Node process or listening socket.
//!
//! A dedicated service thread owns CPAL's platform stream. Binary Tauri IPC is
//! bounded by four ~11ms packet credits. The audio callback only accesses atomics
//! and preallocated storage; it never waits on IPC, allocates, or locks a mutex.
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, SizedSample,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};
use tauri::{ipc::InvokeBody, ipc::Request, WebviewWindow};

#[cfg(test)]
const PACKET_FRAMES: usize = 512;
#[cfg(test)]
const PACKET_BYTES: usize = PACKET_FRAMES * 2 * 4;
const QUEUE_PACKETS: usize = 4;
#[cfg(test)]
const PREFILL_FRAMES: usize = PACKET_FRAMES * 2;
const MAX_PACKET_FRAMES: usize = 4096;
const FADE_SECONDS: f32 = 0.003;
const POLL_TIMEOUT: Duration = Duration::from_millis(100);
const POLL_INTERVAL: Duration = Duration::from_millis(4);

fn packet_frames(sample_rate: u32) -> usize {
    // 128-frame alignment, ~10.7..16ms per packet across supported rates.
    // 48kHz -> 512, 96kHz -> 1024, 192kHz -> 2048, 384kHz -> 4096.
    (sample_rate as usize).div_ceil(12_000) * 128
}

/// Single producer (service thread), single consumer (CPAL callback).
/// Atomic sample storage keeps this queue entirely safe Rust. Publish a whole
/// packet with Release; the callback Acquire observes complete stereo frames.
struct PcmQueue {
    samples: Box<[AtomicU32]>,
    capacity: usize,
    written: AtomicUsize,
    read: AtomicUsize,
    consumed_frames: AtomicU64,
    underruns: AtomicU64,
    failed: AtomicBool,
    stopping: AtomicBool,
    stopped: AtomicBool,
}

impl PcmQueue {
    fn new(capacity: usize) -> Self {
        Self {
            samples: (0..capacity * 2)
                .map(|_| AtomicU32::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            capacity,
            written: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            consumed_frames: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        }
    }

    fn queued(&self) -> usize {
        let read = self.read.load(Ordering::Acquire);
        self.written
            .load(Ordering::Acquire)
            .wrapping_sub(read)
            .min(self.capacity)
    }

    fn push(&self, frames: &[[f32; 2]]) -> Result<(), String> {
        let written = self.written.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        if frames.len() > self.capacity.saturating_sub(written.wrapping_sub(read)) {
            return Err("Native audio queue is full".into());
        }
        for (offset, frame) in frames.iter().enumerate() {
            let index = written.wrapping_add(offset) % self.capacity * 2;
            self.samples[index].store(frame[0].to_bits(), Ordering::Relaxed);
            self.samples[index + 1].store(frame[1].to_bits(), Ordering::Relaxed);
        }
        self.written
            .store(written.wrapping_add(frames.len()), Ordering::Release);
        Ok(())
    }

    fn pop(&self) -> Option<[f32; 2]> {
        let read = self.read.load(Ordering::Relaxed);
        if read == self.written.load(Ordering::Acquire) {
            return None;
        }
        let index = read % self.capacity * 2;
        let frame = [
            f32::from_bits(self.samples[index].load(Ordering::Relaxed)),
            f32::from_bits(self.samples[index + 1].load(Ordering::Relaxed)),
        ];
        self.read.store(read.wrapping_add(1), Ordering::Release);
        self.consumed_frames.fetch_add(1, Ordering::Relaxed);
        Some(frame)
    }
}

struct Renderer {
    queue: Arc<PcmQueue>,
    playing: bool,
    has_played: bool,
    fade_in: usize,
    fade_out: usize,
    last: [f32; 2],
    fade_frames: usize,
    stopping: bool,
}

impl Renderer {
    fn new(queue: Arc<PcmQueue>, sample_rate: u32) -> Self {
        Self {
            queue,
            playing: false,
            has_played: false,
            fade_in: 0,
            fade_out: 0,
            last: [0.0; 2],
            fade_frames: (sample_rate as f32 * FADE_SECONDS).round().max(1.0) as usize,
            stopping: false,
        }
    }

    fn next_frame(&mut self) -> [f32; 2] {
        if self.queue.failed.load(Ordering::Relaxed) {
            return [0.0; 2];
        }
        if self.queue.stopping.load(Ordering::Relaxed) {
            if !self.stopping {
                self.stopping = true;
                self.fade_out = self.fade_frames;
            }
            if self.fade_out > 0 {
                self.fade_out -= 1;
                let gain = self.fade_out as f32 / self.fade_frames as f32;
                return [self.last[0] * gain, self.last[1] * gain];
            }
            self.queue.stopped.store(true, Ordering::Relaxed);
            return [0.0; 2];
        }
        if !self.playing && self.queue.queued() >= self.queue.capacity / 2 {
            self.playing = true;
            self.has_played = true;
            self.fade_in = self.fade_frames;
        }
        if self.playing {
            if let Some(mut frame) = self.queue.pop() {
                if self.fade_in > 0 {
                    let gain =
                        (self.fade_frames - self.fade_in + 1) as f32 / self.fade_frames as f32;
                    frame[0] *= gain;
                    frame[1] *= gain;
                    self.fade_in -= 1;
                }
                self.last = frame;
                return frame;
            }
            self.playing = false;
            self.fade_out = self.fade_frames;
            if self.has_played {
                self.queue.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
        if self.fade_out > 0 {
            self.fade_out -= 1;
            let gain = self.fade_out as f32 / self.fade_frames as f32;
            return [self.last[0] * gain, self.last[1] * gain];
        }
        [0.0; 2]
    }

    fn render<T: SizedSample + FromSample<f32>>(&mut self, output: &mut [T], channels: usize) {
        for frame in output.chunks_mut(channels) {
            let [left, right] = self.next_frame();
            for (index, sample) in frame.iter_mut().enumerate() {
                let value = if channels == 1 {
                    (left + right) * 0.5
                } else {
                    match index {
                        0 => left,
                        1 => right,
                        _ => 0.0,
                    }
                };
                *sample = T::from_sample(value);
            }
        }
    }
}

fn decode_packet(bytes: &[u8], frames: usize) -> Result<Box<[[f32; 2]]>, String> {
    let expected_bytes = frames * 2 * 4;
    if bytes.len() != expected_bytes {
        return Err(format!(
            "Expected {expected_bytes} bytes of stereo Float32 PCM"
        ));
    }
    let mut frames = Vec::with_capacity(frames);
    for bytes in bytes.chunks_exact(8) {
        let left = f32::from_le_bytes(bytes[..4].try_into().unwrap());
        let right = f32::from_le_bytes(bytes[4..].try_into().unwrap());
        if !left.is_finite() || !right.is_finite() {
            return Err("Non-finite native audio sample".into());
        }
        frames.push([left.clamp(-1.0, 1.0), right.clamp(-1.0, 1.0)]);
    }
    Ok(frames.into_boxed_slice())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceInfo {
    id: String,
    name: String,
    channels: u16,
    preferred_sample_rate: u32,
    is_default: bool,
}

struct Output {
    // The service thread creates and drops this platform-specific !Send stream.
    _stream: cpal::Stream,
    session: String,
    queue: Arc<PcmQueue>,
    errors: mpsc::Receiver<String>,
    last_error: Option<String>,
    received_frames: u64,
    credits: PacketCredits,
    sample_rate: u32,
    packet_frames: usize,
}

struct PacketCredits {
    available: usize,
    returned_packets: u64,
    packet_frames: u64,
}

impl PacketCredits {
    fn new(packet_frames: usize) -> Self {
        Self {
            available: QUEUE_PACKETS,
            returned_packets: 0,
            packet_frames: packet_frames as u64,
        }
    }

    fn require_packet(&self) -> Result<(), String> {
        if self.available == 0 {
            return Err("Native audio packet exceeded its credits".into());
        }
        Ok(())
    }

    fn replenish(&mut self, consumed: u64) -> usize {
        let packets = consumed / self.packet_frames;
        let credits = packets.saturating_sub(self.returned_packets) as usize;
        self.returned_packets = packets;
        self.available += credits;
        debug_assert!(self.available <= QUEUE_PACKETS);
        credits
    }
}

fn should_stop(active_session: Option<&str>, requested_session: Option<&str>) -> bool {
    requested_session.is_none() || active_session.is_some() && active_session == requested_session
}

impl Drop for Output {
    fn drop(&mut self) {
        self.queue.stopping.store(true, Ordering::Relaxed);
        // Only the non-realtime service thread waits. Let the callback fade its
        // final sample before dropping the native device stream. A disconnected
        // device may no longer call us, so the wait is strictly bounded.
        for _ in 0..20 {
            if self.queue.stopped.load(Ordering::Relaxed)
                || self.queue.failed.load(Ordering::Relaxed)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Output {
    fn verify_session(&self, session: &str) -> Result<(), String> {
        if self.session != session {
            return Err("Native audio session has ended".into());
        }
        if self.queue.failed.load(Ordering::Relaxed) {
            return Err(self
                .last_error
                .clone()
                .unwrap_or_else(|| "Native audio device failed".into()));
        }
        Ok(())
    }

    fn write(&mut self, session: &str, bytes: &[u8]) -> Result<(), String> {
        self.verify_session(session)?;
        let frames = decode_packet(bytes, self.packet_frames)?;
        self.credits.require_packet()?;
        self.queue.push(&frames)?;
        self.credits.available -= 1;
        self.received_frames += frames.len() as u64;
        Ok(())
    }

    fn poll(&mut self, session: &str) -> Result<Value, String> {
        // A device error remains queryable so the WebView can explain/fallback.
        if self.session != session {
            return Err("Native audio session has ended".into());
        }
        while let Ok(error) = self.errors.try_recv() {
            self.last_error = Some(error);
        }
        let consumed = self.queue.consumed_frames.load(Ordering::Relaxed);
        let credits = self.credits.replenish(consumed);
        Ok(json!({
            "credits": credits,
            "error": self.last_error,
            "receivedFrames": self.received_frames,
            "consumedFrames": consumed,
            "queuedFrames": self.queue.queued(),
            "underruns": self.queue.underruns.load(Ordering::Relaxed),
            "sampleRate": self.sample_rate,
        }))
    }
}

fn build_stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<PcmQueue>,
    errors: mpsc::Sender<String>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    let mut renderer = Renderer::new(queue.clone(), config.sample_rate.0);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| renderer.render(data, channels),
            move |error| {
                queue.failed.store(true, Ordering::Relaxed);
                let _ = errors.send(format!("Native audio output failed: {error}"));
            },
            None,
        )
        .map_err(|error| format!("Cannot open native audio output: {error}"))
}

fn open_output(device: cpal::Device) -> Result<Output, String> {
    // Keep the native device's default rate/format. The WebAudio capture
    // Worklet resamples from its own rate to this explicitly returned rate.
    let supported = device
        .default_output_config()
        .map_err(|error| format!("Cannot read native audio configuration: {error}"))?;
    let sample_rate = supported.sample_rate().0;
    let packet_frames = packet_frames(sample_rate);
    let mut config = supported.config();
    if config.channels == 0 || !(8000..=384_000).contains(&sample_rate) {
        return Err("Native audio device has an unsupported channel count or sample rate".into());
    }
    if let cpal::SupportedBufferSize::Range { min, max } = supported.buffer_size() {
        if *min <= packet_frames as u32 && *max >= packet_frames as u32 {
            config.buffer_size = cpal::BufferSize::Fixed(packet_frames as u32);
        }
    }
    let queue = Arc::new(PcmQueue::new(packet_frames * QUEUE_PACKETS));
    let (send, errors) = mpsc::channel();
    macro_rules! stream {
        ($sample:ty) => {
            build_stream::<$sample>(&device, &config, queue.clone(), send)
        };
    }
    let stream = match supported.sample_format() {
        cpal::SampleFormat::I8 => stream!(i8),
        cpal::SampleFormat::I16 => stream!(i16),
        cpal::SampleFormat::I32 => stream!(i32),
        cpal::SampleFormat::I64 => stream!(i64),
        cpal::SampleFormat::U8 => stream!(u8),
        cpal::SampleFormat::U16 => stream!(u16),
        cpal::SampleFormat::U32 => stream!(u32),
        cpal::SampleFormat::U64 => stream!(u64),
        cpal::SampleFormat::F32 => stream!(f32),
        cpal::SampleFormat::F64 => stream!(f64),
        format => Err(format!("Unsupported native audio sample format: {format}")),
    }?;
    stream
        .play()
        .map_err(|error| format!("Cannot start native audio output: {error}"))?;
    Ok(Output {
        _stream: stream,
        session: uuid::Uuid::new_v4().to_string(),
        queue,
        errors,
        last_error: None,
        received_frames: 0,
        credits: PacketCredits::new(packet_frames),
        sample_rate,
        packet_frames,
    })
}

enum Operation {
    Devices,
    Start {
        sample_rate: u32,
        device_id: Option<String>,
    },
    Write {
        session: String,
        bytes: Box<[u8]>,
    },
    Poll {
        session: String,
    },
    Stop {
        session: Option<String>,
    },
    Shutdown,
}

struct Command {
    operation: Operation,
    reply: mpsc::Sender<Result<Value, String>>,
}

#[derive(Default)]
struct Service {
    sender: Mutex<Option<mpsc::SyncSender<Command>>>,
}

fn wait_for_credits(
    timeout: Duration,
    mut poll: impl FnMut() -> Result<Value, String>,
) -> Result<Value, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let status = poll()?;
        if status["credits"].as_u64().unwrap_or(0) > 0
            || !status["error"].is_null()
            || std::time::Instant::now() >= deadline
        {
            return Ok(status);
        }
        // This is a native blocking-pool thread, not the audio callback or the
        // CPAL owner. No JavaScript timer is needed to replenish stream credits.
        std::thread::sleep(POLL_INTERVAL);
    }
}

impl Service {
    fn poll_credits(&self, session: &str) -> Result<Value, String> {
        wait_for_credits(POLL_TIMEOUT, || {
            self.request(Operation::Poll {
                session: session.to_owned(),
            })
        })
    }
    fn request(&self, operation: Operation) -> Result<Value, String> {
        let sender = {
            let mut state = self.sender.lock().map_err(|error| error.to_string())?;
            if state.is_none() && matches!(operation, Operation::Stop { .. }) {
                return Ok(Value::Null);
            }
            if state.is_none() {
                let (send, receive) = mpsc::sync_channel::<Command>(16);
                std::thread::Builder::new()
                    .name("synth-native-audio".into())
                    .spawn(move || service_loop(receive))
                    .map_err(|error| error.to_string())?;
                *state = Some(send);
            }
            state.as_ref().unwrap().clone()
        };
        let (send, receive) = mpsc::channel();
        sender
            .try_send(Command {
                operation,
                reply: send,
            })
            .map_err(|_| "Native audio command queue is busy".to_string())?;
        receive
            .recv_timeout(Duration::from_secs(10))
            .map_err(|error| format!("Native audio command did not finish: {error}"))?
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Ok(state) = self.sender.get_mut() {
            if let Some(sender) = state.take() {
                let (send, _) = mpsc::channel();
                let _ = sender.try_send(Command {
                    operation: Operation::Shutdown,
                    reply: send,
                });
            }
        }
    }
}

fn service_loop(receive: mpsc::Receiver<Command>) {
    let host = cpal::default_host();
    let mut devices: Vec<(String, cpal::Device)> = Vec::new();
    let mut output: Option<Output> = None;
    while let Ok(command) = receive.recv() {
        if matches!(command.operation, Operation::Shutdown) {
            break;
        }
        let result = (|| -> Result<Value, String> {
            match command.operation {
                Operation::Devices => {
                    let default_name = host
                        .default_output_device()
                        .and_then(|device| device.name().ok());
                    let mut infos = Vec::new();
                    let mut next_devices = Vec::new();
                    let mut marked_default = false;
                    for device in host.output_devices().map_err(|error| error.to_string())? {
                        let Ok(config) = device.default_output_config() else {
                            continue;
                        };
                        if config.channels() == 0 {
                            continue;
                        }
                        let name = device.name().unwrap_or_else(|_| "Audio output".into());
                        let is_default = !marked_default && default_name.as_ref() == Some(&name);
                        marked_default |= is_default;
                        let id = uuid::Uuid::new_v4().to_string();
                        infos.push(DeviceInfo {
                            id: id.clone(),
                            name,
                            channels: config.channels(),
                            preferred_sample_rate: config.sample_rate().0,
                            is_default,
                        });
                        next_devices.push((id, device));
                    }
                    devices = next_devices;
                    serde_json::to_value(infos).map_err(|error| error.to_string())
                }
                Operation::Start {
                    sample_rate,
                    device_id,
                } => {
                    if !(8000..=384_000).contains(&sample_rate) {
                        return Err("Invalid WebAudio sample rate".into());
                    }
                    // Drop the old stream before opening the next one.
                    output = None;
                    let device = if let Some(id) = device_id {
                        let index = devices
                            .iter()
                            .position(|(known, _)| known == &id)
                            .ok_or("Audio device is no longer available; refresh devices")?;
                        devices[index].1.clone()
                    } else {
                        host.default_output_device()
                            .ok_or("No native audio output device is available")?
                    };
                    let started = open_output(device)?;
                    let info = json!({
                        "session": started.session,
                        "frames": started.packet_frames,
                        "sampleRate": started.sample_rate,
                        "inputSampleRate": sample_rate,
                        "credits": QUEUE_PACKETS,
                    });
                    output = Some(started);
                    Ok(info)
                }
                Operation::Write { session, bytes } => {
                    output
                        .as_mut()
                        .ok_or("Native audio output is not active")?
                        .write(&session, &bytes)?;
                    Ok(Value::Null)
                }
                Operation::Poll { session } => output
                    .as_mut()
                    .ok_or("Native audio output is not active")?
                    .poll(&session),
                Operation::Stop { session } => {
                    // A late pagehide/write failure from an old session cannot
                    // stop a newly selected output.
                    if should_stop(
                        output.as_ref().map(|active| active.session.as_str()),
                        session.as_deref(),
                    ) {
                        output = None;
                    }
                    Ok(Value::Null)
                }
                Operation::Shutdown => unreachable!(),
            }
        })();
        let _ = command.reply.send(result);
    }
}

#[derive(Default, Clone)]
pub struct Audio(Arc<Service>);

impl Audio {
    pub fn shutdown(&self) {
        let _ = self.0.request(Operation::Stop { session: None });
    }

    async fn request(&self, operation: Operation) -> Result<Value, String> {
        let audio = self.clone();
        tauri::async_runtime::spawn_blocking(move || audio.0.request(operation))
            .await
            .map_err(|error| error.to_string())?
    }
}

#[tauri::command]
pub async fn audio_devices(
    window: WebviewWindow,
    state: tauri::State<'_, Audio>,
) -> Result<Value, String> {
    super::allowed(&window)?;
    state.request(Operation::Devices).await
}

#[tauri::command]
pub async fn audio_start(
    window: WebviewWindow,
    state: tauri::State<'_, Audio>,
    sample_rate: u32,
    device_id: Option<String>,
) -> Result<Value, String> {
    super::allowed(&window)?;
    state
        .request(Operation::Start {
            sample_rate,
            device_id,
        })
        .await
}

#[tauri::command]
pub async fn audio_write(
    window: WebviewWindow,
    state: tauri::State<'_, Audio>,
    request: Request<'_>,
) -> Result<Value, String> {
    super::allowed(&window)?;
    let session = request
        .headers()
        .get("x-synth-audio-session")
        .and_then(|value| value.to_str().ok())
        .ok_or("Missing native audio session")?
        .to_owned();
    if session.len() != 36 || uuid::Uuid::parse_str(&session).is_err() {
        return Err("Invalid native audio session".into());
    }
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("Native audio requires a binary PCM packet".into());
    };
    if bytes.is_empty() || bytes.len() > MAX_PACKET_FRAMES * 2 * 4 {
        return Err("Native audio packet is too large or empty".into());
    }
    state
        .request(Operation::Write {
            session,
            bytes: bytes.clone().into_boxed_slice(),
        })
        .await
}

#[tauri::command]
pub async fn audio_poll(
    window: WebviewWindow,
    state: tauri::State<'_, Audio>,
    session: String,
) -> Result<Value, String> {
    super::allowed(&window)?;
    let audio = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || audio.0.poll_credits(&session))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn audio_stop(
    window: WebviewWindow,
    state: tauri::State<'_, Audio>,
    session: Option<String>,
) -> Result<Value, String> {
    super::allowed(&window)?;
    state.request(Operation::Stop { session }).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_poll_waits_on_native_time_until_credits_or_deadline() {
        let started = std::time::Instant::now();
        let mut polls = 0;
        let result = wait_for_credits(Duration::from_millis(20), || {
            polls += 1;
            Ok(json!({"credits": 0, "error": null}))
        })
        .unwrap();
        assert_eq!(result["credits"], 0);
        assert!(started.elapsed() >= Duration::from_millis(20));
        assert!(polls < 10, "idle long poll must not busy-spin");
        assert_eq!(
            wait_for_credits(POLL_TIMEOUT, || Ok(json!({"credits": 2, "error": null}))).unwrap()
                ["credits"],
            2
        );
        assert_eq!(
            wait_for_credits(POLL_TIMEOUT, || Ok(
                json!({"credits": 0, "error": "device lost"})
            ))
            .unwrap()["error"],
            "device lost"
        );
        assert!(wait_for_credits(POLL_TIMEOUT, || Err("session ended".into())).is_err());
    }

    #[test]
    fn long_poll_leaves_service_available_for_pcm_writes() {
        let (send, receive) = mpsc::sync_channel::<Command>(16);
        let service = Arc::new(Service {
            sender: Mutex::new(Some(send)),
        });
        let owner = std::thread::spawn(move || {
            let mut available = false;
            while let Ok(command) = receive.recv() {
                let value = match command.operation {
                    Operation::Poll { .. } => {
                        json!({"credits": if available { 1 } else { 0 }, "error": null})
                    }
                    Operation::Write { .. } => {
                        available = true;
                        Value::Null
                    }
                    Operation::Shutdown => break,
                    _ => panic!("unexpected test operation"),
                };
                command.reply.send(Ok(value)).unwrap();
            }
        });
        let waiting = service.clone();
        let poll = std::thread::spawn(move || waiting.poll_credits("current"));
        std::thread::sleep(Duration::from_millis(8));
        service
            .request(Operation::Write {
                session: "current".into(),
                bytes: Box::new([]),
            })
            .unwrap();
        assert_eq!(poll.join().unwrap().unwrap()["credits"], 1);
        drop(service);
        owner.join().unwrap();
    }

    #[test]
    fn packets_keep_a_bounded_duration_at_every_supported_sample_rate() {
        for (rate, expected) in [
            (8000, 128),
            (16_000, 256),
            (44_100, 512),
            (48_000, 512),
            (96_000, 1024),
            (192_000, 2048),
            (384_000, 4096),
        ] {
            let frames = packet_frames(rate);
            assert_eq!(frames, expected);
            assert!(frames <= MAX_PACKET_FRAMES);
            let seconds = frames as f64 / rate as f64;
            assert!((0.010..=0.016).contains(&seconds));
            let bytes = vec![0; frames * 8];
            assert_eq!(decode_packet(&bytes, frames).unwrap().len(), frames);
            assert!(decode_packet(&bytes, frames + 128).is_err());
            let mut credits = PacketCredits::new(frames);
            credits.available = 0;
            assert_eq!(credits.replenish(frames as u64 - 1), 0);
            assert_eq!(credits.replenish(frames as u64), 1);
        }
    }

    #[test]
    fn credits_only_return_once_after_a_whole_packet_is_consumed() {
        let mut credits = PacketCredits::new(PACKET_FRAMES);
        for _ in 0..QUEUE_PACKETS {
            credits.require_packet().unwrap();
            credits.available -= 1;
        }
        assert!(credits.require_packet().is_err());
        assert_eq!(credits.replenish(511), 0);
        assert_eq!(credits.replenish(512), 1);
        assert_eq!(credits.replenish(512), 0);
        assert_eq!(credits.replenish(1023), 0);
        assert_eq!(credits.replenish(2048), 3);
        assert_eq!(credits.available, QUEUE_PACKETS);
    }

    #[test]
    fn late_stop_from_previous_session_does_not_stop_new_output() {
        assert!(!should_stop(Some("new"), Some("old")));
        assert!(should_stop(Some("new"), Some("new")));
        assert!(should_stop(Some("new"), None));
        assert!(!should_stop(None, Some("old")));
    }

    #[test]
    fn stop_fades_last_sample_without_draining_pending_audio() {
        let queue = Arc::new(PcmQueue::new(PREFILL_FRAMES));
        queue.push(&vec![[0.5, -0.5]; PREFILL_FRAMES]).unwrap();
        let mut renderer = Renderer::new(queue.clone(), 48_000);
        for _ in 0..renderer.fade_frames {
            renderer.next_frame();
        }
        let queued = queue.queued();
        queue.stopping.store(true, Ordering::Relaxed);
        let first = renderer.next_frame();
        assert!(first[0] > 0.0 && first[0] < 0.5);
        for _ in 0..renderer.fade_frames {
            renderer.next_frame();
        }
        assert_eq!(renderer.next_frame(), [0.0; 2]);
        assert!(queue.stopped.load(Ordering::Relaxed));
        assert_eq!(queue.queued(), queued);
    }

    /// Opt-in hardware check: plays a quiet (-40 dBFS) tone for 400ms.
    /// `cargo test native_device_smoke -- --ignored --nocapture`
    #[test]
    #[ignore = "opens the system output device and briefly plays low-level audio"]
    fn native_device_smoke() {
        let audio = Audio::default();
        let devices = audio
            .0
            .request(Operation::Devices)
            .expect("enumerate native output devices");
        println!("Native output devices: {devices}");
        let started = audio
            .0
            .request(Operation::Start {
                sample_rate: 48_000,
                device_id: None,
            })
            .expect("start default native output");
        println!("Native output started: {started}");
        let session = started["session"].as_str().unwrap().to_owned();
        let sample_rate = started["sampleRate"].as_u64().unwrap() as f64;
        let mut credits = started["credits"].as_u64().unwrap();
        let frames_per_packet = started["frames"].as_u64().unwrap() as usize;
        let mut frame_index = 0u64;
        let until = std::time::Instant::now() + Duration::from_millis(400);
        while std::time::Instant::now() < until {
            for _ in 0..credits {
                let bytes = (0..frames_per_packet)
                    .map(|_| {
                        let sample = ((frame_index as f64 * 440.0 / sample_rate
                            * std::f64::consts::TAU)
                            .sin()
                            * 0.01) as f32;
                        frame_index += 1;
                        let little_endian = sample.to_le_bytes();
                        [
                            little_endian[0],
                            little_endian[1],
                            little_endian[2],
                            little_endian[3],
                            little_endian[0],
                            little_endian[1],
                            little_endian[2],
                            little_endian[3],
                        ]
                    })
                    .flatten()
                    .collect::<Vec<_>>()
                    .into_boxed_slice();
                audio
                    .0
                    .request(Operation::Write {
                        session: session.clone(),
                        bytes,
                    })
                    .expect("write native PCM");
            }
            let polled = audio
                .0
                .poll_credits(&session)
                .expect("long poll native PCM");
            assert!(polled["error"].is_null(), "{polled}");
            credits = polled["credits"].as_u64().unwrap();
        }
        // Stale cleanup must leave the current stream usable.
        audio
            .0
            .request(Operation::Stop {
                session: Some("old-session".into()),
            })
            .unwrap();
        let status = audio
            .0
            .request(Operation::Poll {
                session: session.clone(),
            })
            .unwrap();
        println!("Native output final status: {status}");
        assert!(status["consumedFrames"].as_u64().unwrap() > PACKET_FRAMES as u64);
        assert_eq!(status["underruns"], 0);
        audio
            .0
            .request(Operation::Stop {
                session: Some(session.clone()),
            })
            .unwrap();
        assert!(audio.0.request(Operation::Poll { session }).is_err());
    }

    #[test]
    fn stereo_packets_are_little_endian_finite_and_clipped() {
        let mut bytes = vec![0; PACKET_BYTES];
        bytes[..4].copy_from_slice(&2.0f32.to_le_bytes());
        bytes[4..8].copy_from_slice(&(-0.25f32).to_le_bytes());
        let frames = decode_packet(&bytes, PACKET_FRAMES).unwrap();
        assert_eq!(frames[0], [1.0, -0.25]);
        assert!(decode_packet(&bytes[..8], PACKET_FRAMES).is_err());
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            bytes[..4].copy_from_slice(&invalid.to_le_bytes());
            assert!(decode_packet(&bytes, PACKET_FRAMES).is_err());
        }
    }

    #[test]
    fn queue_is_bounded_preserves_stereo_order_and_wraps() {
        let queue = PcmQueue::new(3);
        queue
            .push(&[[0.1, -0.1], [0.2, -0.2], [0.3, -0.3]])
            .unwrap();
        assert_eq!(queue.queued(), 3);
        assert!(queue.push(&[[0.9, 0.9]]).is_err());
        assert_eq!(queue.pop(), Some([0.1, -0.1]));
        queue.push(&[[0.4, -0.4]]).unwrap();
        for frame in [[0.2, -0.2], [0.3, -0.3], [0.4, -0.4]] {
            assert_eq!(queue.pop(), Some(frame));
        }
        assert_eq!(queue.pop(), None);
        assert_eq!(queue.consumed_frames.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn queue_keeps_concurrent_packets_intact() {
        let queue = Arc::new(PcmQueue::new(2048));
        let producer = queue.clone();
        let writer = std::thread::spawn(move || {
            for number in 0..50_000 {
                let frame = [[number as f32, -(number as f32)]];
                while producer.push(&frame).is_err() {
                    std::thread::yield_now()
                }
            }
        });
        for number in 0..50_000 {
            loop {
                if let Some(frame) = queue.pop() {
                    assert_eq!(frame, [number as f32, -(number as f32)]);
                    break;
                }
                std::thread::yield_now();
            }
        }
        writer.join().unwrap();
    }

    #[test]
    fn renderer_prefills_fades_in_and_out_and_recovers() {
        let queue = Arc::new(PcmQueue::new(PACKET_FRAMES * QUEUE_PACKETS));
        let mut renderer = Renderer::new(queue.clone(), 48_000);
        let fade_frames = renderer.fade_frames;
        queue.push(&vec![[0.5, -0.5]; PACKET_FRAMES]).unwrap();
        assert_eq!(renderer.next_frame(), [0.0; 2]);
        assert_eq!(queue.queued(), PACKET_FRAMES);
        queue.push(&vec![[0.5, -0.5]; PACKET_FRAMES]).unwrap();
        let first = renderer.next_frame();
        assert_eq!(first, [0.5 / fade_frames as f32, -0.5 / fade_frames as f32]);
        for _ in 1..PREFILL_FRAMES {
            renderer.next_frame();
        }
        let first_gap = renderer.next_frame();
        assert!(first_gap[0] > 0.0 && first_gap[0] < 0.5);
        for _ in 1..fade_frames {
            renderer.next_frame();
        }
        assert_eq!(renderer.next_frame(), [0.0; 2]);
        assert_eq!(queue.underruns.load(Ordering::Relaxed), 1);
        queue.push(&vec![[0.25, 0.25]; PREFILL_FRAMES]).unwrap();
        assert_eq!(renderer.next_frame(), [0.25 / fade_frames as f32; 2]);
    }

    #[test]
    fn renderer_maps_stereo_mono_and_extra_output_channels() {
        for (channels, expected) in [
            (1, vec![0.25]),
            (2, vec![0.75, -0.25]),
            (4, vec![0.75, -0.25, 0.0, 0.0]),
        ] {
            let queue = Arc::new(PcmQueue::new(PREFILL_FRAMES));
            queue.push(&vec![[0.75, -0.25]; PREFILL_FRAMES]).unwrap();
            let mut renderer = Renderer::new(queue, 48_000);
            let fade_frames = renderer.fade_frames;
            for _ in 0..fade_frames {
                renderer.next_frame();
            }
            let mut output = vec![99.0; channels];
            renderer.render(&mut output, channels);
            assert_eq!(output, expected);
        }
    }
}
