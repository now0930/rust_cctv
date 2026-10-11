use rust_cctv::{
    config::ServiceConfig,
    events::TrackEventEmitter,
    ingest::{run_rtsp, IngestMessage, LatestFrames},
    storage::{EventStore, SqliteEventStore},
    tracking::{BoundingBox, Detection as TrackDetection, IouTracker},
    vision::{Detection, TinyYoloV2Detector, CLASS_NAMES},
};
use std::{
    error::Error,
    fs, process,
    sync::atomic::{AtomicBool, Ordering},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_REIDENTIFY_MISSED_FRAMES: u32 = 60;
const TRACK_MAX_MISSED_FRAMES: u32 = 15;
const TRACK_MIN_HITS: u64 = 5;
const TRACK_MIN_IOU: f32 = 0.3;
const TRACK_REIDENTIFY_MIN_IOU: f32 = 0.5;
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn handle_shutdown_signal(_: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

fn main() {
    if let Err(error) = run() {
        eprintln!("event=service_error error={error:?}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let Some(config) = ServiceConfig::from_env_and_args()? else {
        print_usage();
        return Ok(());
    };

    if let Some(parent) = config
        .database_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    install_shutdown_handlers()?;

    let store = SqliteEventStore::open(&config.database_path)?;
    let integrity = store.integrity_check()?;
    if integrity.len() != 1 || integrity[0] != "ok" {
        return Err(format!("SQLite integrity check failed: {}", integrity.join("; ")).into());
    }
    let detector = config
        .model_path
        .as_ref()
        .map(TinyYoloV2Detector::load)
        .transpose()?;
    let event_count = store.count()?;
    println!(
        "event=service_ready service=rust-cctv camera_id={:?} database={:?} events={event_count} input={} detector={} score_threshold={:.2} inference_fps={:.1} model_input={}x{}",
        config.camera_id,
        config.database_path,
        if config.rtsp_url.is_some() {
            "configured"
        } else {
            "not_configured"
        },
        if detector.is_some() { "tinyyolov2" } else { "disabled" },
        config.score_threshold,
        config.inference_fps,
        rust_cctv::vision::INPUT_WIDTH,
        rust_cctv::vision::INPUT_HEIGHT,
    );

    let latest_frames = detector.as_ref().map(|_| LatestFrames::new());
    let (messages_tx, messages_rx) = mpsc::channel();
    let ingest_worker = config.rtsp_url.map(|url| {
        let frame_queue = latest_frames.clone();
        thread::spawn(move || run_rtsp(url, &SHUTDOWN_REQUESTED, messages_tx, frame_queue))
    });

    let run_id = format!(
        "{}-{}",
        process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
    );
    let mut tracker = IouTracker::new(
        TRACK_MAX_MISSED_FRAMES,
        TRACK_MIN_IOU,
        TRACK_MIN_HITS,
        MAX_REIDENTIFY_MISSED_FRAMES,
        TRACK_REIDENTIFY_MIN_IOU,
    );
    let mut event_emitter = TrackEventEmitter::new(run_id);
    let mut analytics = AnalyticsWindow::new();
    let inference_interval = Duration::from_secs_f64(1.0 / config.inference_fps);
    let mut next_inference_at = Instant::now();

    while !SHUTDOWN_REQUESTED.load(Ordering::Relaxed) {
        drain_ingest_messages(&messages_rx, &config.camera_id);
        if detector.is_some() {
            let now = Instant::now();
            if now < next_inference_at {
                let wait = (next_inference_at - now).min(Duration::from_millis(100));
                match messages_rx.recv_timeout(wait) {
                    Ok(message) => log_ingest_message(message, &config.camera_id),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {}
                }
                continue;
            }
        }
        let frame = latest_frames.as_ref().and_then(LatestFrames::take_latest);
        if let (Some(detector), Some(frame)) = (&detector, frame) {
            let inference_started = Instant::now();
            next_inference_at = inference_started + inference_interval;
            let detections = detector.detect_rgb(&frame.rgb, config.score_threshold)?;
            let inference_elapsed = inference_started.elapsed();
            let frame_age = frame.received_at.elapsed();
            let tracks = to_track_detections(&detections);
            tracker.update(&tracks);
            let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
            for event in event_emitter.emit_transitions(
                &tracker,
                &config.camera_id,
                timestamp_ms,
                MAX_REIDENTIFY_MISSED_FRAMES,
                &CLASS_NAMES,
            ) {
                store.save(&event)?;
                analytics.events_saved += 1;
            }
            analytics.record(
                &detections,
                inference_elapsed,
                frame_age,
                tracker.active_confirmed_count(),
            );
            if analytics.elapsed() >= Duration::from_secs(5) {
                log_analytics(
                    &analytics,
                    latest_frames.as_ref(),
                    store.count()?,
                    &config.camera_id,
                );
                analytics = AnalyticsWindow::new();
            }
        } else {
            match messages_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(message) => log_ingest_message(message, &config.camera_id),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    if let Some(worker) = ingest_worker {
        worker.join().map_err(|_| "RTSP ingest worker panicked")?;
    }
    drop(store);
    println!("event=service_stopped service=rust-cctv reason=signal");
    Ok(())
}

fn print_usage() {
    println!("usage: rust-cctv [--db-path PATH] [--camera-id ID] [--rtsp-url URL] [--model-path PATH] [--score-threshold N] [--inference-fps N]");
    println!("environment: RUST_CCTV_DB_PATH, RUST_CCTV_CAMERA_ID, RUST_CCTV_RTSP_URL, RUST_CCTV_MODEL_PATH, RUST_CCTV_SCORE_THRESHOLD, RUST_CCTV_INFERENCE_FPS");
}

fn drain_ingest_messages(messages: &Receiver<IngestMessage>, camera_id: &str) {
    while let Ok(message) = messages.try_recv() {
        log_ingest_message(message, camera_id);
    }
}

fn log_ingest_message(message: IngestMessage, camera_id: &str) {
    match message {
        IngestMessage::Connected {
            width,
            height,
            format,
        } => println!(
            "event=rtsp_connected camera_id={camera_id:?} source={width}x{height} pixel_format={format:?} motion_analysis={}x{}",
            rust_cctv::ingest::ANALYSIS_WIDTH,
            rust_cctv::ingest::ANALYSIS_HEIGHT,
        ),
        IngestMessage::Metrics {
            frames,
            replaced_frames,
            motion_frames,
            window_fps,
            average_ms,
            average_gray_difference,
            peak_gray_difference,
        } => println!(
            "event=ingest_metrics camera_id={camera_id:?} decoded_frames={frames} replaced_frames={replaced_frames} window_fps={window_fps:.1} motion_frames={motion_frames} gray_diff_avg={average_gray_difference:.2} gray_diff_peak={peak_gray_difference} gray_diff_avg_ms={average_ms:.3}"
        ),
        IngestMessage::Reconnecting {
            delay_seconds,
            error,
        } => eprintln!(
            "event=rtsp_retry level=warn camera_id={camera_id:?} delay_seconds={delay_seconds} reason={error:?}"
        ),
    }
}

fn to_track_detections(detections: &[Detection]) -> Vec<TrackDetection> {
    detections
        .iter()
        .map(|detection| TrackDetection {
            class_id: detection.class_id,
            confidence: detection.confidence,
            bbox: BoundingBox {
                x_min: detection.x_min,
                y_min: detection.y_min,
                x_max: detection.x_max,
                y_max: detection.y_max,
            },
        })
        .collect()
}

struct AnalyticsWindow {
    started: Instant,
    inference_times: Vec<Duration>,
    frame_age_times: Vec<Duration>,
    person_frames: u64,
    dog_frames: u64,
    events_saved: u64,
    active_confirmed_tracks: usize,
}

impl AnalyticsWindow {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            inference_times: Vec::new(),
            frame_age_times: Vec::new(),
            person_frames: 0,
            dog_frames: 0,
            events_saved: 0,
            active_confirmed_tracks: 0,
        }
    }

    fn record(
        &mut self,
        detections: &[Detection],
        inference_time: Duration,
        frame_age: Duration,
        active_tracks: usize,
    ) {
        self.inference_times.push(inference_time);
        self.frame_age_times.push(frame_age);
        self.person_frames += u64::from(detections.iter().any(|d| d.class_id == 14));
        self.dog_frames += u64::from(detections.iter().any(|d| d.class_id == 11));
        self.active_confirmed_tracks = active_tracks;
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

fn log_analytics(
    analytics: &AnalyticsWindow,
    frames: Option<&LatestFrames>,
    events_total: u64,
    camera_id: &str,
) {
    let inference_count = analytics.inference_times.len().max(1);
    let frame_age_count = analytics.frame_age_times.len().max(1);
    let inference_avg_ms = analytics
        .inference_times
        .iter()
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .sum::<f64>()
        / inference_count as f64;
    let frame_age_avg_ms = analytics
        .frame_age_times
        .iter()
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .sum::<f64>()
        / frame_age_count as f64;
    let elapsed = analytics.started.elapsed().as_secs_f64().max(0.001);
    let (decoded_frames, replaced_frames) = frames.map_or((0, 0), LatestFrames::counts);
    println!(
        "event=analytics_metrics camera_id={camera_id:?} inference_frames={} inference_fps={:.1} inference_avg_ms={inference_avg_ms:.2} frame_age_avg_ms={frame_age_avg_ms:.2} person_hit_frames={} dog_hit_frames={} active_confirmed_tracks={} events_saved={} events_total={} decoded_frames={decoded_frames} replaced_frames={replaced_frames}",
        analytics.inference_times.len(),
        analytics.inference_times.len() as f64 / elapsed,
        analytics.person_frames,
        analytics.dog_frames,
        analytics.active_confirmed_tracks,
        analytics.events_saved,
        events_total,
    );
}

#[cfg(unix)]
fn install_shutdown_handlers() -> Result<(), Box<dyn Error>> {
    unsafe {
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::signal(
                signal,
                handle_shutdown_signal as *const () as libc::sighandler_t,
            ) == libc::SIG_ERR
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn install_shutdown_handlers() -> Result<(), Box<dyn Error>> {
    Err("signal handling is currently supported on Unix systems only".into())
}
