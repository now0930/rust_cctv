use ffmpeg::{
    codec, format, frame, media,
    software::scaling::{context::Context as Scaler, flag::Flags},
};
use ffmpeg_next as ffmpeg;
use rust_cctv::{
    detection::DetectionEventKind,
    events::TrackEventEmitter,
    storage::{EventStore, InMemoryEventStore, SqliteEventStore},
    tracking::{BoundingBox, Detection as TrackDetection, IouTracker},
};
use std::{
    env,
    error::Error,
    process,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tract_onnx::prelude::*;

const INPUT_WIDTH: usize = 416;
const INPUT_HEIGHT: usize = 416;
const GRID: usize = 13;
const VALUES_PER_BOX: usize = 25;
const MAX_REIDENTIFY_MISSED_FRAMES: u32 = 60;
const CLASS_NAMES: [&str; 20] = [
    "aeroplane",
    "bicycle",
    "bird",
    "boat",
    "bottle",
    "bus",
    "car",
    "cat",
    "chair",
    "cow",
    "diningtable",
    "dog",
    "horse",
    "motorbike",
    "person",
    "pottedplant",
    "sheep",
    "sofa",
    "train",
    "tvmonitor",
];
const ANCHORS: [(f32, f32); 5] = [
    (1.08, 1.19),
    (3.42, 4.41),
    (6.63, 11.38),
    (9.42, 5.11),
    (16.62, 10.52),
];

struct Detection {
    class_id: usize,
    confidence: f32,
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
}

struct FrameInput {
    pixels: Vec<f32>,
    received_at: Instant,
}

#[derive(Default)]
struct FrameQueueState {
    latest: Option<FrameInput>,
    decoded: u64,
    replaced: u64,
    finished: bool,
    error: Option<String>,
}

struct LatestFrames {
    shared: Arc<(Mutex<FrameQueueState>, Condvar)>,
}

struct RtspFrames {
    input: format::context::Input,
    video_index: usize,
    decoder: ffmpeg::decoder::Video,
    scaler: Scaler,
    decoded: frame::Video,
    scaled: frame::Video,
    demux_eof: bool,
    decoder_flushed: bool,
}

impl RtspFrames {
    fn open(url: &str) -> Result<Self, Box<dyn Error>> {
        ffmpeg::init()?;
        let input = format::input(url)?;
        let video_stream = input
            .streams()
            .best(media::Type::Video)
            .ok_or("no video stream found")?;
        let video_index = video_stream.index();
        let decoder_context = codec::context::Context::from_parameters(video_stream.parameters())?;
        let decoder = decoder_context.decoder().video()?;
        drop(video_stream);
        let scaler = Scaler::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            format::Pixel::RGB24,
            INPUT_WIDTH as u32,
            INPUT_HEIGHT as u32,
            Flags::BILINEAR,
        )?;
        Ok(Self {
            input,
            video_index,
            decoder,
            scaler,
            decoded: frame::Video::empty(),
            scaled: frame::Video::empty(),
            demux_eof: false,
            decoder_flushed: false,
        })
    }

    fn next_pixels(&mut self) -> Result<Option<FrameInput>, Box<dyn Error>> {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => {
                    let received_at = Instant::now();
                    self.scaler.run(&self.decoded, &mut self.scaled)?;
                    let rgb = self.scaled.data(0);
                    let stride = self.scaled.stride(0);
                    let plane_size = INPUT_WIDTH * INPUT_HEIGHT;
                    let mut chw = vec![0.0f32; plane_size * 3];
                    for y in 0..INPUT_HEIGHT {
                        for x in 0..INPUT_WIDTH {
                            let source = y * stride + x * 3;
                            let target = y * INPUT_WIDTH + x;
                            chw[target] = rgb[source] as f32 / 255.0;
                            chw[plane_size + target] = rgb[source + 1] as f32 / 255.0;
                            chw[plane_size * 2 + target] = rgb[source + 2] as f32 / 255.0;
                        }
                    }
                    return Ok(Some(FrameInput {
                        pixels: chw,
                        received_at,
                    }));
                }
                Err(ffmpeg::Error::Eof) => return Ok(None),
                Err(_) => {}
            }

            if self.demux_eof {
                if !self.decoder_flushed {
                    self.decoder.send_eof()?;
                    self.decoder_flushed = true;
                    continue;
                }
                return Ok(None);
            }

            let packet = self.input.packets().find_map(|(stream, packet)| {
                (stream.index() == self.video_index).then_some(packet)
            });
            match packet {
                Some(packet) => self.decoder.send_packet(&packet)?,
                None => self.demux_eof = true,
            }
        }
    }
}

impl LatestFrames {
    fn connect(url: String) -> Self {
        let shared = Arc::new((Mutex::new(FrameQueueState::default()), Condvar::new()));
        let producer_shared = Arc::clone(&shared);
        thread::spawn(move || {
            let mut source = match RtspFrames::open(&url) {
                Ok(source) => source,
                Err(error) => {
                    finish_reader(&producer_shared, Some(error.to_string()));
                    return;
                }
            };
            loop {
                let (lock, _) = &*producer_shared;
                if lock.lock().map(|state| state.finished).unwrap_or(true) {
                    return;
                }
                match source.next_pixels() {
                    Ok(Some(frame)) => {
                        let (lock, ready) = &*producer_shared;
                        let Ok(mut state) = lock.lock() else {
                            return;
                        };
                        if state.finished {
                            return;
                        }
                        if state.latest.replace(frame).is_some() {
                            state.replaced += 1;
                        }
                        state.decoded += 1;
                        ready.notify_one();
                    }
                    Ok(None) => {
                        finish_reader(&producer_shared, None);
                        return;
                    }
                    Err(error) => {
                        finish_reader(&producer_shared, Some(error.to_string()));
                        return;
                    }
                }
            }
        });
        Self { shared }
    }

    fn next(&self) -> Result<FrameInput, Box<dyn Error>> {
        let (lock, ready) = &*self.shared;
        let mut state = lock.lock().map_err(|_| "frame queue lock poisoned")?;
        loop {
            if let Some(frame) = state.latest.take() {
                return Ok(frame);
            }
            if state.finished {
                return Err(state
                    .error
                    .clone()
                    .unwrap_or_else(|| "RTSP reader stopped".to_owned())
                    .into());
            }
            state = ready.wait(state).map_err(|_| "frame queue lock poisoned")?;
        }
    }

    fn counts(&self) -> Result<(u64, u64), Box<dyn Error>> {
        let (lock, _) = &*self.shared;
        let state = lock.lock().map_err(|_| "frame queue lock poisoned")?;
        Ok((state.decoded, state.replaced))
    }
}

impl Drop for LatestFrames {
    fn drop(&mut self) {
        finish_reader(&self.shared, None);
    }
}

fn finish_reader(shared: &Arc<(Mutex<FrameQueueState>, Condvar)>, error: Option<String>) {
    let (lock, ready) = &**shared;
    if let Ok(mut state) = lock.lock() {
        state.finished = true;
        state.error = error;
        ready.notify_all();
    }
}

fn sigmoid(value: f32) -> f32 {
    1.0 / (1.0 + (-value).exp())
}

fn decode_tiny_yolov2(output: &Tensor, threshold: f32) -> Result<Vec<Detection>, Box<dyn Error>> {
    if output.shape() != [1, 125, 13, 13] {
        return Err(format!("unexpected Tiny YOLOv2 output shape: {:?}", output.shape()).into());
    }
    let output_view = output.to_plain_array_view::<f32>()?;
    let values = output_view
        .as_slice()
        .ok_or("YOLO output tensor is not contiguous")?;
    let cell_count = GRID * GRID;
    let value = |anchor: usize, attribute: usize, cell: usize| {
        values[(anchor * VALUES_PER_BOX + attribute) * cell_count + cell]
    };
    let mut detections = Vec::new();

    for y in 0..GRID {
        for x in 0..GRID {
            let cell = y * GRID + x;
            for (anchor, (anchor_width, anchor_height)) in ANCHORS.iter().copied().enumerate() {
                let class_logits: [f32; 20] =
                    std::array::from_fn(|class_id| value(anchor, 5 + class_id, cell));
                let max_logit = class_logits
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max);
                let class_exp: [f32; 20] = class_logits.map(|logit| (logit - max_logit).exp());
                let class_sum: f32 = class_exp.iter().sum();
                let center_x = (x as f32 + sigmoid(value(anchor, 0, cell))) / GRID as f32;
                let center_y = (y as f32 + sigmoid(value(anchor, 1, cell))) / GRID as f32;
                let width =
                    value(anchor, 2, cell).clamp(-10.0, 10.0).exp() * anchor_width / GRID as f32;
                let height =
                    value(anchor, 3, cell).clamp(-10.0, 10.0).exp() * anchor_height / GRID as f32;
                for class_id in [11usize, 14] {
                    let confidence =
                        sigmoid(value(anchor, 4, cell)) * class_exp[class_id] / class_sum;
                    if confidence >= threshold {
                        detections.push(Detection {
                            class_id,
                            confidence,
                            x_min: (center_x - width / 2.0).clamp(0.0, 1.0),
                            y_min: (center_y - height / 2.0).clamp(0.0, 1.0),
                            x_max: (center_x + width / 2.0).clamp(0.0, 1.0),
                            y_max: (center_y + height / 2.0).clamp(0.0, 1.0),
                        });
                    }
                }
            }
        }
    }
    Ok(non_max_suppression(detections, 0.45))
}

fn top_class_confidences(output: &Tensor) -> Result<[f32; 20], Box<dyn Error>> {
    if output.shape() != [1, 125, 13, 13] {
        return Err(format!("unexpected Tiny YOLOv2 output shape: {:?}", output.shape()).into());
    }
    let output_view = output.to_plain_array_view::<f32>()?;
    let values = output_view
        .as_slice()
        .ok_or("YOLO output tensor is not contiguous")?;
    let cell_count = GRID * GRID;
    let value = |anchor: usize, attribute: usize, cell: usize| {
        values[(anchor * VALUES_PER_BOX + attribute) * cell_count + cell]
    };
    let mut maxima = [0.0f32; CLASS_NAMES.len()];
    for y in 0..GRID {
        for x in 0..GRID {
            let cell = y * GRID + x;
            for anchor in 0..ANCHORS.len() {
                let logits: [f32; 20] =
                    std::array::from_fn(|class_id| value(anchor, 5 + class_id, cell));
                let max_logit = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let exponentials = logits.map(|logit| (logit - max_logit).exp());
                let denominator: f32 = exponentials.iter().sum();
                let objectness = sigmoid(value(anchor, 4, cell));
                for class_id in 0..CLASS_NAMES.len() {
                    maxima[class_id] =
                        maxima[class_id].max(objectness * exponentials[class_id] / denominator);
                }
            }
        }
    }
    Ok(maxima)
}

fn non_max_suppression(mut detections: Vec<Detection>, threshold: f32) -> Vec<Detection> {
    detections.sort_by(|left, right| right.confidence.total_cmp(&left.confidence));
    let mut kept: Vec<Detection> = Vec::new();
    for candidate in detections {
        let suppressed = kept.iter().any(|accepted| {
            accepted.class_id == candidate.class_id
                && intersection_over_union(accepted, &candidate) > threshold
        });
        if !suppressed {
            kept.push(candidate);
        }
    }
    kept
}

fn intersection_over_union(left: &Detection, right: &Detection) -> f32 {
    let width = (left.x_max.min(right.x_max) - left.x_min.max(right.x_min)).max(0.0);
    let height = (left.y_max.min(right.y_max) - left.y_min.max(right.y_min)).max(0.0);
    let intersection = width * height;
    let left_area = (left.x_max - left.x_min).max(0.0) * (left.y_max - left.y_min).max(0.0);
    let right_area = (right.x_max - right.x_min).max(0.0) * (right.y_max - right.y_min).max(0.0);
    intersection / (left_area + right_area - intersection).max(f32::EPSILON)
}

fn image_tensor(
    frame: &FrameInput,
    scale: f32,
    swap_red_blue: bool,
) -> Result<Tensor, Box<dyn Error>> {
    let plane_size = INPUT_WIDTH * INPUT_HEIGHT;
    let mut pixels = frame.pixels.clone();
    if swap_red_blue {
        for index in 0..plane_size {
            pixels.swap(index, 2 * plane_size + index);
        }
    }
    if scale != 1.0 {
        pixels.iter_mut().for_each(|value| *value *= scale);
    }
    Ok(Tensor::from_shape(
        &[1, 3, INPUT_HEIGHT, INPUT_WIDTH],
        &pixels,
    )?)
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let model_path = args
        .next()
        .ok_or("usage: model-probe <model.onnx> [iterations=30] [rtsp-url|video:path|image:path] [score-threshold=0.25] [input-scale=255] [rgb|bgr]")?;
    let iterations: usize = args.next().map_or(Ok(30), |value| value.parse())?;
    let source_arg = args.next();
    let score_threshold: f32 = args.next().map_or(Ok(0.25), |value| value.parse())?;
    let input_scale: f32 = args.next().map_or(Ok(255.0), |value| value.parse())?;
    let swap_red_blue = match args.next().as_deref().unwrap_or("rgb") {
        "rgb" => false,
        "bgr" => true,
        other => return Err(format!("unsupported color order: {other}").into()),
    };
    if iterations == 0 {
        return Err("iterations must be greater than zero".into());
    }
    if !(0.0..=1.0).contains(&score_threshold) {
        return Err("score threshold must be between 0 and 1".into());
    }

    let model = tract_onnx::onnx()
        .model_for_path(&model_path)?
        .into_optimized()?
        .into_runnable()?;
    let (input, input_description) = match model.input_fact(0)?.datum_type {
        DatumType::U8 => (
            Tensor::from_shape(&[1, 300, 300, 3], &vec![0u8; 300 * 300 * 3])?,
            "uint8 NHWC 300x300",
        ),
        DatumType::F32 => (
            Tensor::from_shape(&[1, 3, 416, 416], &vec![0f32; 3 * 416 * 416])?,
            "float32 NCHW 416x416",
        ),
        datum_type => return Err(format!("unsupported model input type: {datum_type:?}").into()),
    };
    let synthetic_input = input;
    if source_arg.is_some() && model.input_fact(0)?.datum_type != DatumType::F32 {
        return Err("image/RTSP input currently requires float32 NCHW 416x416 model input".into());
    }
    let image_frame = match source_arg
        .as_deref()
        .filter(|source| source.starts_with("image:"))
    {
        Some(source) => Some(
            RtspFrames::open(&source[6..])?
                .next_pixels()?
                .ok_or("image source contains no decodable video frame")?,
        ),
        None => None,
    };
    let mut video_frames = source_arg
        .as_deref()
        .and_then(|source| source.strip_prefix("video:"))
        .map(RtspFrames::open)
        .transpose()?;
    let latest_frames = source_arg
        .filter(|source| !source.starts_with("image:") && !source.starts_with("video:"))
        .map(LatestFrames::connect);

    for _ in 0..3 {
        let frame = match &latest_frames {
            Some(source) => Some(source.next()?),
            None => match video_frames.as_mut() {
                Some(source) => Some(
                    source
                        .next_pixels()?
                        .ok_or("video ended before model warmup completed")?,
                ),
                None => image_frame.as_ref().map(|frame| FrameInput {
                    pixels: frame.pixels.clone(),
                    received_at: Instant::now(),
                }),
            },
        };
        let input = match frame.as_ref() {
            Some(frame) => image_tensor(frame, input_scale, swap_red_blue)?,
            None => match image_frame.as_ref() {
                Some(frame) => {
                    Tensor::from_shape(&[1, 3, INPUT_HEIGHT, INPUT_WIDTH], &frame.pixels)?
                }
                None => synthetic_input.clone(),
            },
        };
        let outputs = model.run(tvec!(input.into()))?;
        if outputs
            .first()
            .is_some_and(|output| output.shape() == [1, 125, 13, 13])
        {
            let _ = decode_tiny_yolov2(&outputs[0], score_threshold)?;
        }
    }

    let stream_start = latest_frames
        .as_ref()
        .map(LatestFrames::counts)
        .transpose()?;
    let started = Instant::now();
    let mut last_shapes = Vec::new();
    let mut last_detections = Vec::new();
    let mut max_class_confidences = [0.0f32; CLASS_NAMES.len()];
    let mut person_hit_frames = 0usize;
    let mut dog_hit_frames = 0usize;
    // At ~12 inference iterations/s, retain identities for about 1.25 seconds
    // so brief detector dropouts do not create a new ID for the same subject.
    let mut tracker = IouTracker::new(15, 0.3, 5, MAX_REIDENTIFY_MISSED_FRAMES, 0.5);
    let run_id = format!(
        "{}-{}",
        process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis()
    );
    let mut event_emitter = TrackEventEmitter::new(run_id);
    let event_store = InMemoryEventStore::new(10_000);
    let sqlite_event_store = env::var_os("RUST_CCTV_EVENT_DB")
        .map(SqliteEventStore::open)
        .transpose()?;
    let mut sqlite_events_written = 0usize;
    let mut peak_active_tracks = 0usize;
    let mut latencies = Vec::with_capacity(iterations);
    let mut processed_iterations = 0usize;
    for _ in 0..iterations {
        let frame = match &latest_frames {
            Some(source) => Some(source.next()?),
            None => match video_frames.as_mut() {
                Some(source) => match source.next_pixels()? {
                    Some(frame) => Some(frame),
                    None => break,
                },
                None => image_frame.as_ref().map(|frame| FrameInput {
                    pixels: frame.pixels.clone(),
                    received_at: Instant::now(),
                }),
            },
        };
        let received_at = frame.as_ref().map(|frame| frame.received_at);
        let input = match frame.as_ref() {
            Some(frame) => image_tensor(frame, input_scale, swap_red_blue)?,
            None => match image_frame.as_ref() {
                Some(frame) => {
                    Tensor::from_shape(&[1, 3, INPUT_HEIGHT, INPUT_WIDTH], &frame.pixels)?
                }
                None => synthetic_input.clone(),
            },
        };
        let outputs = model.run(tvec!(input.into()))?;
        last_shapes = outputs
            .iter()
            .map(|tensor| format!("{:?}", tensor.shape()))
            .collect();
        if outputs
            .first()
            .is_some_and(|output| output.shape() == [1, 125, 13, 13])
        {
            let class_confidences = top_class_confidences(&outputs[0])?;
            if class_confidences[14] >= score_threshold {
                person_hit_frames += 1;
            }
            if class_confidences[11] >= score_threshold {
                dog_hit_frames += 1;
            }
            for (maximum, confidence) in max_class_confidences.iter_mut().zip(class_confidences) {
                *maximum = maximum.max(confidence);
            }
            last_detections = decode_tiny_yolov2(&outputs[0], score_threshold)?;
            let tracker_detections: Vec<_> = last_detections
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
                .collect();
            let tracks = tracker.update(&tracker_detections);
            peak_active_tracks = peak_active_tracks.max(tracks.len());
            let timestamp_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
            for event in event_emitter.emit_transitions(
                &tracker,
                "logitech_c922",
                timestamp_ms,
                MAX_REIDENTIFY_MISSED_FRAMES,
                &CLASS_NAMES,
            ) {
                event_store.save(&event)?;
                if let Some(store) = &sqlite_event_store {
                    store.save(&event)?;
                    sqlite_events_written += 1;
                }
            }
        }
        if let Some(received_at) = received_at {
            latencies.push(received_at.elapsed());
        }
        processed_iterations += 1;
    }
    if processed_iterations == 0 {
        return Err("input ended before any measured frames were processed".into());
    }
    let elapsed = started.elapsed();
    latencies.sort_unstable();
    let p95_ms = latencies
        .get((latencies.len() * 95).div_ceil(100).saturating_sub(1))
        .map(|latency| latency.as_secs_f64() * 1000.0)
        .unwrap_or_default();
    let stream_counts = latest_frames
        .as_ref()
        .map(LatestFrames::counts)
        .transpose()?;
    let (read_frames, dropped_frames) = match (stream_start, stream_counts) {
        (Some((read_start, dropped_start)), Some((read_end, dropped_end))) => {
            (read_end - read_start, dropped_end - dropped_start)
        }
        _ => (0, 0),
    };
    let person_count = last_detections
        .iter()
        .filter(|detection| detection.class_id == 14)
        .count();
    let dog_count = last_detections
        .iter()
        .filter(|detection| detection.class_id == 11)
        .count();
    let max_confidence = last_detections
        .iter()
        .map(|detection| detection.confidence)
        .fold(0.0f32, f32::max);
    let lifecycle_events = event_store.all()?;
    let confirmed_events = lifecycle_events
        .iter()
        .filter(|event| event.kind == DetectionEventKind::TrackConfirmed)
        .count();
    let lost_events = lifecycle_events
        .iter()
        .filter(|event| event.kind == DetectionEventKind::TrackLost)
        .count();
    let sqlite_events_total = sqlite_event_store
        .as_ref()
        .map(SqliteEventStore::count)
        .transpose()?
        .unwrap_or_default();
    let sqlite_events_read = sqlite_event_store
        .as_ref()
        .map(|store| store.list_after(0, 10_000))
        .transpose()?
        .map_or(0, |events| events.len());
    println!(
        "input={input_description} source={} scale={input_scale} color_order={} iterations={processed_iterations} total_ms={:.2} avg_ms={:.2} p95_ms={p95_ms:.2} fps={:.2} stream_frames={read_frames} replaced_frames={dropped_frames} last_persons={person_count} last_dogs={dog_count} last_max_confidence={max_confidence:.3} person_hit_frames={person_hit_frames} dog_hit_frames={dog_hit_frames} tracks_created={} tracks_reidentified={} active_confirmed_tracks={} confirmed_tracks_total={} track_confirmed_events={confirmed_events} track_lost_events={lost_events} sqlite_events_written={sqlite_events_written} sqlite_events_read={sqlite_events_read} sqlite_events_total={sqlite_events_total} peak_active_tracks={peak_active_tracks} outputs={}",
        if latest_frames.is_some() {
            "rtsp-latest"
        } else if video_frames.is_some() {
            "video-sequential"
        } else if image_frame.is_some() {
            "image"
        } else {
            "zeros"
        },
        if swap_red_blue { "bgr" } else { "rgb" },
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0 / processed_iterations as f64,
        processed_iterations as f64 / elapsed.as_secs_f64(),
        tracker.created_count(),
        tracker.reidentified_count(),
        tracker.active_confirmed_count(),
        tracker.confirmed_count(),
        last_shapes.join(",")
    );
    for track in tracker.history() {
        println!(
            "track id={} class={} active={} confirmed={} hits={} missed={} confidence={:.3} box=({:.3},{:.3},{:.3},{:.3})",
            track.id,
            CLASS_NAMES[track.class_id],
            tracker.is_active(track.id),
            track.confirmed,
            track.hits,
            track.missed,
            track.confidence,
            track.bbox.x_min,
            track.bbox.y_min,
            track.bbox.x_max,
            track.bbox.y_max
        );
    }
    for detection in last_detections {
        println!(
            "detection class={} confidence={:.3} box=({:.3},{:.3},{:.3},{:.3})",
            CLASS_NAMES[detection.class_id],
            detection.confidence,
            detection.x_min,
            detection.y_min,
            detection.x_max,
            detection.y_max
        );
    }
    let mut ranked_classes: Vec<_> = max_class_confidences.into_iter().enumerate().collect();
    ranked_classes.sort_by(|left, right| right.1.total_cmp(&left.1));
    for (class_id, confidence) in ranked_classes.into_iter().take(5) {
        println!(
            "window_top_class={} max_confidence={confidence:.6}",
            CLASS_NAMES[class_id]
        );
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("model-probe: {error}");
        process::exit(1);
    }
}
