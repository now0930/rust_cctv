use crate::tracking::BoundingBox;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectionEventKind {
    TrackConfirmed,
    TrackLost,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DetectionEvent {
    pub run_id: String,
    pub camera_id: String,
    pub track_id: u64,
    pub kind: DetectionEventKind,
    pub label: String,
    pub confidence: f32,
    pub timestamp_ms: u64,
    pub bbox: BoundingBox,
}
