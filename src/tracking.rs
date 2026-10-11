use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingBox {
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub class_id: usize,
    pub confidence: f32,
    pub bbox: BoundingBox,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: u64,
    pub class_id: usize,
    pub confidence: f32,
    pub bbox: BoundingBox,
    pub hits: u64,
    pub missed: u32,
    pub confirmed: bool,
}

pub struct IouTracker {
    tracks: Vec<Track>,
    finished_tracks: Vec<Track>,
    next_id: u64,
    max_missed: u32,
    min_iou: f32,
    min_hits: u64,
    max_reidentify_missed: u32,
    min_reidentify_iou: f32,
    reidentified_count: u64,
}

impl IouTracker {
    pub fn new(
        max_missed: u32,
        min_iou: f32,
        min_hits: u64,
        max_reidentify_missed: u32,
        min_reidentify_iou: f32,
    ) -> Self {
        Self {
            tracks: Vec::new(),
            finished_tracks: Vec::new(),
            next_id: 1,
            max_missed,
            min_iou: min_iou.clamp(0.0, 1.0),
            min_hits: min_hits.max(1),
            max_reidentify_missed,
            min_reidentify_iou: min_reidentify_iou.clamp(0.0, 1.0),
            reidentified_count: 0,
        }
    }

    pub fn update(&mut self, detections: &[Detection]) -> &[Track] {
        for track in &mut self.finished_tracks {
            track.missed = track.missed.saturating_add(1);
        }
        for track in &mut self.tracks {
            track.missed = track.missed.saturating_add(1);
        }

        let mut detection_order: Vec<_> = (0..detections.len()).collect();
        detection_order.sort_by(|left, right| {
            detections[*right]
                .confidence
                .total_cmp(&detections[*left].confidence)
        });
        let mut matched_tracks = HashSet::new();

        for detection_index in detection_order {
            let detection = detections[detection_index];
            let best_match = self
                .tracks
                .iter()
                .enumerate()
                .filter(|(track_index, track)| {
                    track.class_id == detection.class_id
                        && !matched_tracks.contains(track_index)
                        && track.missed <= self.max_missed
                })
                .map(|(track_index, track)| {
                    let iou = intersection_over_union(track.bbox, detection.bbox);
                    let center_distance = center_distance(track.bbox, detection.bbox);
                    let area_ratio = area_ratio(track.bbox, detection.bbox);
                    let nearby_similar_box = area_ratio >= 0.5
                        && area_ratio <= 2.0
                        && center_distance <= center_gate(track.bbox, detection.bbox);
                    (track_index, iou, nearby_similar_box)
                })
                .filter(|(_, iou, nearby_similar_box)| *iou >= self.min_iou || *nearby_similar_box)
                .max_by(|left, right| match left.2.cmp(&right.2) {
                    std::cmp::Ordering::Equal => left.1.total_cmp(&right.1),
                    ordering => ordering,
                });

            if let Some((track_index, _, _)) = best_match {
                let track = &mut self.tracks[track_index];
                track.bbox = detection.bbox;
                track.confidence = detection.confidence;
                track.hits += 1;
                track.missed = 0;
                track.confirmed = track.hits >= self.min_hits;
                matched_tracks.insert(track_index);
            } else {
                let best_reidentification = self
                    .finished_tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, track)| {
                        track.confirmed
                            && track.class_id == detection.class_id
                            && track.missed <= self.max_reidentify_missed
                    })
                    .map(|(track_index, track)| {
                        (
                            track_index,
                            intersection_over_union(track.bbox, detection.bbox),
                        )
                    })
                    .filter(|(_, iou)| *iou >= self.min_reidentify_iou)
                    .max_by(|left, right| left.1.total_cmp(&right.1));

                if let Some((track_index, _)) = best_reidentification {
                    let mut track = self.finished_tracks.remove(track_index);
                    track.bbox = detection.bbox;
                    track.confidence = detection.confidence;
                    track.hits += 1;
                    track.missed = 0;
                    self.reidentified_count += 1;
                    let track_index = self.tracks.len();
                    self.tracks.push(track);
                    matched_tracks.insert(track_index);
                    continue;
                }

                let track_index = self.tracks.len();
                self.tracks.push(Track {
                    id: self.next_id,
                    class_id: detection.class_id,
                    confidence: detection.confidence,
                    bbox: detection.bbox,
                    hits: 1,
                    missed: 0,
                    confirmed: self.min_hits == 1,
                });
                self.next_id += 1;
                matched_tracks.insert(track_index);
            }
        }

        let mut active_tracks = Vec::with_capacity(self.tracks.len());
        for track in self.tracks.drain(..) {
            if track.missed <= self.max_missed {
                active_tracks.push(track);
            } else {
                self.finished_tracks.push(track);
            }
        }
        self.tracks = active_tracks;
        &self.tracks
    }

    pub fn created_count(&self) -> u64 {
        self.next_id - 1
    }

    pub fn reidentified_count(&self) -> u64 {
        self.reidentified_count
    }

    pub fn history(&self) -> impl Iterator<Item = &Track> {
        self.finished_tracks.iter().chain(self.tracks.iter())
    }

    pub fn confirmed_count(&self) -> usize {
        self.history().filter(|track| track.confirmed).count()
    }

    pub fn active_confirmed_count(&self) -> usize {
        self.tracks.iter().filter(|track| track.confirmed).count()
    }

    pub fn is_active(&self, track_id: u64) -> bool {
        self.tracks.iter().any(|track| track.id == track_id)
    }
}

fn intersection_over_union(left: BoundingBox, right: BoundingBox) -> f32 {
    let width = (left.x_max.min(right.x_max) - left.x_min.max(right.x_min)).max(0.0);
    let height = (left.y_max.min(right.y_max) - left.y_min.max(right.y_min)).max(0.0);
    let intersection = width * height;
    let left_area = (left.x_max - left.x_min).max(0.0) * (left.y_max - left.y_min).max(0.0);
    let right_area = (right.x_max - right.x_min).max(0.0) * (right.y_max - right.y_min).max(0.0);
    intersection / (left_area + right_area - intersection).max(f32::EPSILON)
}

fn center_distance(left: BoundingBox, right: BoundingBox) -> f32 {
    let left_x = (left.x_min + left.x_max) * 0.5;
    let left_y = (left.y_min + left.y_max) * 0.5;
    let right_x = (right.x_min + right.x_max) * 0.5;
    let right_y = (right.y_min + right.y_max) * 0.5;
    (left_x - right_x).hypot(left_y - right_y)
}

fn area_ratio(left: BoundingBox, right: BoundingBox) -> f32 {
    let left_area = box_area(left);
    let right_area = box_area(right);
    if left_area <= f32::EPSILON || right_area <= f32::EPSILON {
        return 0.0;
    }
    (left_area / right_area).min(right_area / left_area)
}

fn box_area(bbox: BoundingBox) -> f32 {
    (bbox.x_max - bbox.x_min).max(0.0) * (bbox.y_max - bbox.y_min).max(0.0)
}

fn center_gate(left: BoundingBox, right: BoundingBox) -> f32 {
    let left_diagonal = box_diagonal(left);
    let right_diagonal = box_diagonal(right);
    (left_diagonal.max(right_diagonal) * 0.5).clamp(0.04, 0.12)
}

fn box_diagonal(bbox: BoundingBox) -> f32 {
    let width = (bbox.x_max - bbox.x_min).max(0.0);
    let height = (bbox.y_max - bbox.y_min).max(0.0);
    width.hypot(height)
}
