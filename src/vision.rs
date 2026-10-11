use std::{error::Error, path::Path, sync::Arc};
use tract_onnx::prelude::*;

pub const INPUT_WIDTH: usize = 416;
pub const INPUT_HEIGHT: usize = 416;
pub const CLASS_NAMES: [&str; 20] = [
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

const GRID: usize = 13;
const VALUES_PER_BOX: usize = 25;
const ANCHORS: [(f32, f32); 5] = [
    (1.08, 1.19),
    (3.42, 4.41),
    (6.63, 11.38),
    (9.42, 5.11),
    (16.62, 10.52),
];

#[derive(Debug, Clone, Copy)]
pub struct Detection {
    pub class_id: usize,
    pub confidence: f32,
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
}

pub struct TinyYoloV2Detector {
    model: Arc<TypedRunnableModel>,
}

impl TinyYoloV2Detector {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Box<dyn Error>> {
        let model = tract_onnx::onnx()
            .model_for_path(path)?
            .into_optimized()?
            .into_runnable()?;
        if model.input_fact(0)?.datum_type != DatumType::F32 {
            return Err("Tiny YOLOv2 service expects a float32 model input".into());
        }
        let detector = Self { model };
        let warmup = Tensor::from_shape(
            &[1, 3, INPUT_HEIGHT, INPUT_WIDTH],
            &vec![0.0f32; 3 * INPUT_WIDTH * INPUT_HEIGHT],
        )?;
        for _ in 0..3 {
            detector.model.run(tvec!(warmup.clone().into()))?;
        }
        Ok(detector)
    }

    pub fn detect_rgb(
        &self,
        rgb: &[u8],
        score_threshold: f32,
    ) -> Result<Vec<Detection>, Box<dyn Error>> {
        let plane_size = INPUT_WIDTH * INPUT_HEIGHT;
        if rgb.len() != plane_size * 3 {
            return Err(format!(
                "expected packed RGB frame with {} bytes, got {}",
                plane_size * 3,
                rgb.len()
            )
            .into());
        }
        let mut chw = vec![0.0f32; plane_size * 3];
        for index in 0..plane_size {
            chw[index] = rgb[index * 3] as f32;
            chw[plane_size + index] = rgb[index * 3 + 1] as f32;
            chw[plane_size * 2 + index] = rgb[index * 3 + 2] as f32;
        }
        let input = Tensor::from_shape(&[1, 3, INPUT_HEIGHT, INPUT_WIDTH], &chw)?;
        let outputs = self.model.run(tvec!(input.into()))?;
        let output = outputs.first().ok_or("model returned no output")?;
        decode_tiny_yolov2(output, score_threshold)
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
