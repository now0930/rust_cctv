use ffmpeg::{
    codec, format, frame, media,
    software::scaling::{context::Context as Scaler, flag::Flags},
};
use ffmpeg_next as ffmpeg;
use rust_cctv::vision::{Detection, TinyYoloV2Detector, CLASS_NAMES};
use std::{
    collections::{BTreeMap, HashSet},
    env,
    error::Error,
    path::{Path, PathBuf},
};

const PERSON: usize = 14;
const DOG: usize = 11;
const INPUT_WIDTH: u32 = 416;
const INPUT_HEIGHT: u32 = 416;

#[derive(Clone, Copy)]
struct BoxLabel {
    class_id: usize,
    x_min: f32,
    y_min: f32,
    x_max: f32,
    y_max: f32,
}

struct FrameLabels {
    scene: String,
    image_path: PathBuf,
    boxes: Vec<BoxLabel>,
}

#[derive(Default, Clone, Copy)]
struct Counts {
    truth: u64,
    detections: u64,
    true_positive: u64,
}

impl Counts {
    fn add(&mut self, truth: u64, detections: u64, true_positive: u64) {
        self.truth += truth;
        self.detections += detections;
        self.true_positive += true_positive;
    }

    fn report(self, name: &str) {
        let false_positive = self.detections - self.true_positive;
        let false_negative = self.truth - self.true_positive;
        let precision = ratio(self.true_positive, self.detections);
        let recall = ratio(self.true_positive, self.truth);
        let f1 = if precision + recall == 0.0 {
            0.0
        } else {
            2.0 * precision * recall / (precision + recall)
        };
        println!(
            "class={name} ground_truth={} detections={} tp={} fp={false_positive} fn={false_negative} precision={precision:.4} recall={recall:.4} f1={f1:.4}",
            self.truth, self.detections, self.true_positive
        );
    }
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn load_labels(path: &str) -> Result<BTreeMap<String, FrameLabels>, Box<dyn Error>> {
    let contents = std::fs::read_to_string(path)?;
    let manifest_directory = Path::new(path).parent().unwrap_or_else(|| Path::new("."));
    let mut frames = BTreeMap::<String, FrameLabels>::new();
    for (line_number, line) in contents.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with("frame_id\t") {
            continue;
        }
        let columns: Vec<_> = line.split('\t').collect();
        if columns.len() != 8 {
            return Err(format!(
                "{}:{}: expected 8 tab-separated columns, got {}",
                path,
                line_number + 1,
                columns.len()
            )
            .into());
        }
        let frame_id = columns[0].to_owned();
        let scene = columns[1].to_owned();
        let image_path = PathBuf::from(columns[2]);
        let image_path = if image_path.is_absolute() {
            image_path
        } else {
            manifest_directory.join(image_path)
        };
        if frame_id.is_empty() || scene.is_empty() || !image_path.is_file() {
            return Err(format!(
                "{}:{}: frame ID, scene and readable image path are required",
                path,
                line_number + 1
            )
            .into());
        }

        let frame = frames
            .entry(frame_id.clone())
            .or_insert_with(|| FrameLabels {
                scene: scene.clone(),
                image_path: image_path.clone(),
                boxes: Vec::new(),
            });
        if frame.scene != scene || frame.image_path != image_path {
            return Err(format!(
                "{}:{}: repeated frame ID has inconsistent scene or image path",
                path,
                line_number + 1
            )
            .into());
        }

        if columns[3].is_empty() {
            if columns[4..].iter().any(|value| !value.is_empty()) {
                return Err(format!(
                    "{}:{}: empty class requires empty box coordinates",
                    path,
                    line_number + 1
                )
                .into());
            }
            continue;
        }
        let class_id = match columns[3] {
            "person" => PERSON,
            "dog" => DOG,
            other => {
                return Err(
                    format!("unsupported class {other:?} at line {}", line_number + 1).into(),
                )
            }
        };
        let mut coordinates = [0.0f32; 4];
        for (target, source) in coordinates.iter_mut().zip(&columns[4..]) {
            *target = source.parse()?;
        }
        if coordinates
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
            || coordinates[0] >= coordinates[2]
            || coordinates[1] >= coordinates[3]
        {
            return Err(format!("invalid normalized box at {}:{}", path, line_number + 1).into());
        }
        frame.boxes.push(BoxLabel {
            class_id,
            x_min: coordinates[0],
            y_min: coordinates[1],
            x_max: coordinates[2],
            y_max: coordinates[3],
        });
    }
    if frames.is_empty() {
        return Err("label manifest contains no frames".into());
    }
    Ok(frames)
}

fn detect_first_frame(
    detector: &TinyYoloV2Detector,
    image_path: &PathBuf,
    score_threshold: f32,
) -> Result<Vec<rust_cctv::vision::Detection>, Box<dyn Error>> {
    ffmpeg::init()?;
    let mut input = format::input(image_path)?;
    let video_stream = input
        .streams()
        .best(media::Type::Video)
        .ok_or("image has no video stream")?;
    let video_index = video_stream.index();
    let context = codec::context::Context::from_parameters(video_stream.parameters())?;
    let mut decoder = context.decoder().video()?;
    drop(video_stream);
    let mut scaler = Scaler::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        format::Pixel::RGB24,
        INPUT_WIDTH,
        INPUT_HEIGHT,
        Flags::BILINEAR,
    )?;
    let mut decoded = frame::Video::empty();
    let mut rgb_scaled = frame::Video::empty();
    loop {
        match decoder.receive_frame(&mut decoded) {
            Ok(()) => {
                scaler.run(&decoded, &mut rgb_scaled)?;
                let rgb = rgb_scaled.data(0);
                let stride = rgb_scaled.stride(0);
                let row_size = INPUT_WIDTH as usize * 3;
                let mut packed = Vec::with_capacity(row_size * INPUT_HEIGHT as usize);
                for y in 0..INPUT_HEIGHT as usize {
                    packed.extend_from_slice(&rgb[y * stride..y * stride + row_size]);
                }
                return detector.detect_rgb(&packed, score_threshold);
            }
            Err(ffmpeg::Error::Eof) => return Err("image contains no decodable frame".into()),
            Err(ffmpeg::Error::Other { .. }) => {}
            Err(error) => return Err(error.into()),
        }
        let packet = input
            .packets()
            .find_map(|(stream, packet)| (stream.index() == video_index).then_some(packet));
        match packet {
            Some(packet) => decoder.send_packet(&packet)?,
            None => decoder.send_eof()?,
        }
    }
}

fn box_iou(left: BoxLabel, right: BoxLabel) -> f32 {
    let width = (left.x_max.min(right.x_max) - left.x_min.max(right.x_min)).max(0.0);
    let height = (left.y_max.min(right.y_max) - left.y_min.max(right.y_min)).max(0.0);
    let intersection = width * height;
    let left_area = (left.x_max - left.x_min) * (left.y_max - left.y_min);
    let right_area = (right.x_max - right.x_min) * (right.y_max - right.y_min);
    intersection / (left_area + right_area - intersection).max(f32::EPSILON)
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let model_path = args.next().ok_or(
        "usage: accuracy-eval <model.onnx> <labels.tsv> [score-threshold=0.25] [match-iou=0.5]",
    )?;
    let labels_path = args.next().ok_or("labels.tsv is required")?;
    let score_threshold = args.next().map_or(Ok(0.25f32), |value| value.parse())?;
    let match_iou = args.next().map_or(Ok(0.5f32), |value| value.parse())?;
    if !(0.0..=1.0).contains(&score_threshold) || !(0.0..=1.0).contains(&match_iou) {
        return Err("score threshold and match IoU must be between 0 and 1".into());
    }
    if args.next().is_some() {
        return Err("too many arguments".into());
    }

    let frames = load_labels(&labels_path)?;
    let detector = TinyYoloV2Detector::load(model_path)?;
    let mut by_scene_class = BTreeMap::<(String, usize), Counts>::new();
    for (frame_id, frame) in &frames {
        let detections = detect_first_frame(&detector, &frame.image_path, score_threshold)?;
        for class_id in [PERSON, DOG] {
            let mut class_detections: Vec<&Detection> = detections
                .iter()
                .filter(|detection| detection.class_id == class_id)
                .collect();
            class_detections.sort_by(|left, right| right.confidence.total_cmp(&left.confidence));
            let truths: Vec<_> = frame
                .boxes
                .iter()
                .copied()
                .filter(|label| label.class_id == class_id)
                .collect();
            let mut matched = HashSet::new();
            let mut true_positive = 0u64;
            for detection in &class_detections {
                let detected_box = BoxLabel {
                    class_id,
                    x_min: detection.x_min,
                    y_min: detection.y_min,
                    x_max: detection.x_max,
                    y_max: detection.y_max,
                };
                let best = truths
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| !matched.contains(index))
                    .map(|(index, truth)| (index, box_iou(detected_box, *truth)))
                    .filter(|(_, iou)| *iou >= match_iou)
                    .max_by(|left, right| left.1.total_cmp(&right.1));
                if let Some((index, _)) = best {
                    matched.insert(index);
                    true_positive += 1;
                }
            }
            by_scene_class
                .entry((frame.scene.clone(), class_id))
                .or_default()
                .add(
                    truths.len() as u64,
                    class_detections.len() as u64,
                    true_positive,
                );
        }
        println!(
            "frame={frame_id} scene={:?} labels={}",
            frame.scene,
            frame.boxes.len()
        );
    }

    let mut overall = BTreeMap::<usize, Counts>::new();
    for ((scene, class_id), counts) in by_scene_class {
        println!("scene={scene:?}");
        counts.report(CLASS_NAMES[class_id]);
        overall.entry(class_id).or_default().add(
            counts.truth,
            counts.detections,
            counts.true_positive,
        );
    }
    println!(
        "summary frames={} score_threshold={score_threshold:.2} match_iou={match_iou:.2}",
        frames.len()
    );
    for (class_id, counts) in overall {
        counts.report(CLASS_NAMES[class_id]);
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("accuracy-eval: {error}");
        std::process::exit(1);
    }
}
