#[derive(Debug, Clone, PartialEq)]
pub struct DetectionEvent {
    pub camera_id: String,
    pub label: String,
    pub confidence: f32,
    pub timestamp: u64,
}
