use ffmpeg::{
    codec, format, frame, media,
    software::scaling::{context::Context as Scaler, flag::Flags},
};
use ffmpeg_next as ffmpeg;
use std::{
    error::Error,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub const ANALYSIS_WIDTH: u32 = 320;
pub const ANALYSIS_HEIGHT: u32 = 180;
pub const MODEL_WIDTH: usize = 416;
pub const MODEL_HEIGHT: usize = 416;

pub struct LatestFrame {
    pub rgb: Vec<u8>,
    pub received_at: Instant,
}

#[derive(Default)]
struct FrameQueueState {
    latest: Option<LatestFrame>,
    decoded: u64,
    replaced: u64,
}

#[derive(Clone)]
pub struct LatestFrames {
    shared: Arc<Mutex<FrameQueueState>>,
}

impl LatestFrames {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(FrameQueueState::default())),
        }
    }

    pub fn take_latest(&self) -> Option<LatestFrame> {
        self.shared.lock().ok()?.latest.take()
    }

    fn publish(&self, frame: LatestFrame) {
        if let Ok(mut state) = self.shared.lock() {
            if state.latest.replace(frame).is_some() {
                state.replaced += 1;
            }
            state.decoded += 1;
        }
    }

    pub fn counts(&self) -> (u64, u64) {
        self.shared
            .lock()
            .map(|state| (state.decoded, state.replaced))
            .unwrap_or_default()
    }
}

#[derive(Debug)]
pub enum IngestMessage {
    Connected {
        width: u32,
        height: u32,
        format: String,
    },
    Metrics {
        frames: u64,
        replaced_frames: u64,
        motion_frames: u64,
        window_fps: f64,
        average_ms: f64,
        average_gray_difference: f64,
        peak_gray_difference: u64,
    },
    Reconnecting {
        delay_seconds: u64,
        error: String,
    },
}

pub fn run_rtsp(
    url: String,
    shutdown: &'static AtomicBool,
    messages: Sender<IngestMessage>,
    latest_frames: Option<LatestFrames>,
) {
    let mut retry_seconds = 1u64;
    while !shutdown.load(Ordering::Relaxed) {
        let mut connected = false;
        let result = run_session(
            &url,
            shutdown,
            &messages,
            &mut connected,
            latest_frames.as_ref(),
        );
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        if connected {
            retry_seconds = 1;
        }

        let error = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "RTSP stream ended".to_owned());
        let _ = messages.send(IngestMessage::Reconnecting {
            delay_seconds: retry_seconds,
            error,
        });
        sleep_or_shutdown(shutdown, Duration::from_secs(retry_seconds));
        retry_seconds = (retry_seconds * 2).min(30);
    }
}

fn run_session(
    url: &str,
    shutdown: &AtomicBool,
    messages: &Sender<IngestMessage>,
    connected: &mut bool,
    latest_frames: Option<&LatestFrames>,
) -> Result<(), Box<dyn Error>> {
    ffmpeg::init()?;
    let mut options = ffmpeg::Dictionary::new();
    options.set("rtsp_transport", "tcp");
    options.set("rw_timeout", "5000000");
    let mut input = format::input_with_dictionary(url, options)?;
    let video_stream = input
        .streams()
        .best(media::Type::Video)
        .ok_or("RTSP input has no video stream")?;
    let video_index = video_stream.index();
    let decoder_context = codec::context::Context::from_parameters(video_stream.parameters())?;
    let mut decoder = decoder_context.decoder().video()?;
    drop(video_stream);

    let source_width = decoder.width();
    let source_height = decoder.height();
    let source_format = format!("{:?}", decoder.format());
    let mut scaler = Scaler::get(
        decoder.format(),
        source_width,
        source_height,
        format::Pixel::GRAY8,
        ANALYSIS_WIDTH,
        ANALYSIS_HEIGHT,
        Flags::BILINEAR,
    )?;
    let mut rgb_scaler = latest_frames
        .map(|_| {
            Scaler::get(
                decoder.format(),
                source_width,
                source_height,
                format::Pixel::RGB24,
                MODEL_WIDTH as u32,
                MODEL_HEIGHT as u32,
                Flags::BILINEAR,
            )
        })
        .transpose()?;
    let _ = messages.send(IngestMessage::Connected {
        width: source_width,
        height: source_height,
        format: source_format,
    });
    *connected = true;

    let mut decoded = frame::Video::empty();
    let mut scaled = frame::Video::empty();
    let mut rgb_scaled = frame::Video::empty();
    let mut previous = vec![0u8; ANALYSIS_WIDTH as usize * ANALYSIS_HEIGHT as usize];
    let mut frames = 0u64;
    let mut motion_frames = 0u64;
    let mut interval_frames = 0u64;
    let mut interval_comparisons = 0u64;
    let mut interval_difference_total = 0u64;
    let mut interval_difference_peak = 0u64;
    let mut processing_total = Duration::ZERO;
    let mut interval_started = Instant::now();

    while !shutdown.load(Ordering::Relaxed) {
        let packet = input
            .packets()
            .find_map(|(stream, packet)| (stream.index() == video_index).then_some(packet));
        let Some(packet) = packet else {
            return Err("RTSP packet stream ended".into());
        };
        decoder.send_packet(&packet)?;

        while decoder.receive_frame(&mut decoded).is_ok() {
            let received_at = Instant::now();
            let processing_started = Instant::now();
            scaler.run(&decoded, &mut scaled)?;
            let pixels = scaled.data(0);
            let stride = scaled.stride(0);
            let mut total_difference = 0u64;
            let mut index = 0usize;
            for y in 0..ANALYSIS_HEIGHT as usize {
                for x in 0..ANALYSIS_WIDTH as usize {
                    let value = pixels[y * stride + x];
                    if frames > 0 {
                        total_difference += value.abs_diff(previous[index]) as u64;
                    }
                    previous[index] = value;
                    index += 1;
                }
            }
            if frames > 0 {
                let average_difference =
                    total_difference / (ANALYSIS_WIDTH as u64 * ANALYSIS_HEIGHT as u64);
                interval_comparisons += 1;
                interval_difference_total += average_difference;
                interval_difference_peak = interval_difference_peak.max(average_difference);
                if average_difference > 5 {
                    motion_frames += 1;
                }
            }
            frames += 1;
            interval_frames += 1;
            processing_total += processing_started.elapsed();

            if let (Some(scaler), Some(latest_frames)) = (&mut rgb_scaler, latest_frames) {
                scaler.run(&decoded, &mut rgb_scaled)?;
                let rgb = rgb_scaled.data(0);
                let stride = rgb_scaled.stride(0);
                let row_size = MODEL_WIDTH * 3;
                let mut packed = Vec::with_capacity(row_size * MODEL_HEIGHT);
                for y in 0..MODEL_HEIGHT {
                    let row_start = y * stride;
                    packed.extend_from_slice(&rgb[row_start..row_start + row_size]);
                }
                latest_frames.publish(LatestFrame {
                    rgb: packed,
                    received_at,
                });
            }

            if interval_started.elapsed() >= Duration::from_secs(5) {
                let average_ms = if interval_frames == 0 {
                    0.0
                } else {
                    processing_total.as_secs_f64() * 1000.0 / interval_frames as f64
                };
                let window_seconds = interval_started.elapsed().as_secs_f64().max(0.001);
                let average_gray_difference = if interval_comparisons == 0 {
                    0.0
                } else {
                    interval_difference_total as f64 / interval_comparisons as f64
                };
                let _ = messages.send(IngestMessage::Metrics {
                    frames,
                    replaced_frames: latest_frames.map_or(0, |frames| frames.counts().1),
                    motion_frames,
                    window_fps: interval_frames as f64 / window_seconds,
                    average_ms,
                    average_gray_difference,
                    peak_gray_difference: interval_difference_peak,
                });
                interval_frames = 0;
                interval_comparisons = 0;
                interval_difference_total = 0;
                interval_difference_peak = 0;
                processing_total = Duration::ZERO;
                interval_started = Instant::now();
            }
        }
    }
    Ok(())
}

fn sleep_or_shutdown(shutdown: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !shutdown.load(Ordering::Relaxed) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
}
