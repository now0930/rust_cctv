use crate::{
    detection::{DetectionEvent, DetectionEventKind},
    tracking::IouTracker,
};
use std::collections::HashMap;

#[derive(Default)]
struct TrackEventState {
    confirmed_emitted: bool,
    lost_emitted: bool,
}

pub struct TrackEventEmitter {
    states: HashMap<u64, TrackEventState>,
    run_id: String,
}

impl TrackEventEmitter {
    pub fn new(run_id: String) -> Self {
        Self {
            states: HashMap::new(),
            run_id,
        }
    }

    pub fn emit_transitions(
        &mut self,
        tracker: &IouTracker,
        camera_id: &str,
        timestamp_ms: u64,
        max_reidentify_missed: u32,
        class_names: &[&str],
    ) -> Vec<DetectionEvent> {
        let mut events = Vec::new();
        for track in tracker.history() {
            let state = self.states.entry(track.id).or_default();
            if !track.confirmed {
                continue;
            }

            if !state.confirmed_emitted {
                state.confirmed_emitted = true;
                events.push(DetectionEvent {
                    run_id: self.run_id.clone(),
                    camera_id: camera_id.to_owned(),
                    track_id: track.id,
                    kind: DetectionEventKind::TrackConfirmed,
                    label: class_names[track.class_id].to_owned(),
                    confidence: track.confidence,
                    timestamp_ms,
                    bbox: track.bbox,
                });
            }

            if !tracker.is_active(track.id)
                && track.missed > max_reidentify_missed
                && !state.lost_emitted
            {
                state.lost_emitted = true;
                events.push(DetectionEvent {
                    run_id: self.run_id.clone(),
                    camera_id: camera_id.to_owned(),
                    track_id: track.id,
                    kind: DetectionEventKind::TrackLost,
                    label: class_names[track.class_id].to_owned(),
                    confidence: track.confidence,
                    timestamp_ms,
                    bbox: track.bbox,
                });
            }
        }
        events
    }
}
