use crate::detection::DetectionEvent;

pub trait EventStore {
    type Error;

    fn save(&self, event: &DetectionEvent) -> Result<(), Self::Error>;
}
