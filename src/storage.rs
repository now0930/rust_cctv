use crate::detection::{DetectionEvent, DetectionEventKind};
use rusqlite::{params, types::Type, Connection, OpenFlags};
use std::{collections::VecDeque, error::Error, fmt, path::Path, sync::Mutex, time::Duration};

pub trait EventStore {
    type Error;

    fn save(&self, event: &DetectionEvent) -> Result<(), Self::Error>;
    fn list_after(
        &self,
        timestamp_ms: u64,
        limit: usize,
    ) -> Result<Vec<DetectionEvent>, Self::Error>;
}

#[derive(Debug)]
pub enum InMemoryStoreError {
    LockPoisoned,
}

impl fmt::Display for InMemoryStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => formatter.write_str("in-memory event store lock poisoned"),
        }
    }
}

impl Error for InMemoryStoreError {}

pub struct InMemoryEventStore {
    capacity: usize,
    events: Mutex<VecDeque<DetectionEvent>>,
}

impl InMemoryEventStore {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            events: Mutex::new(VecDeque::new()),
        }
    }

    pub fn all(&self) -> Result<Vec<DetectionEvent>, InMemoryStoreError> {
        self.events
            .lock()
            .map(|events| events.iter().cloned().collect())
            .map_err(|_| InMemoryStoreError::LockPoisoned)
    }
}

impl EventStore for InMemoryEventStore {
    type Error = InMemoryStoreError;

    fn save(&self, event: &DetectionEvent) -> Result<(), Self::Error> {
        let mut events = self
            .events
            .lock()
            .map_err(|_| InMemoryStoreError::LockPoisoned)?;
        if events.len() == self.capacity {
            events.pop_front();
        }
        events.push_back(event.clone());
        Ok(())
    }

    fn list_after(
        &self,
        timestamp_ms: u64,
        limit: usize,
    ) -> Result<Vec<DetectionEvent>, Self::Error> {
        let events = self
            .events
            .lock()
            .map_err(|_| InMemoryStoreError::LockPoisoned)?;
        Ok(events
            .iter()
            .filter(|event| event.timestamp_ms > timestamp_ms)
            .take(limit)
            .cloned()
            .collect())
    }
}

pub struct SqliteEventStore {
    connection: Mutex<Connection>,
}

impl SqliteEventStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, rusqlite::Error> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS detection_events (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               run_id TEXT NOT NULL,
               camera_id TEXT NOT NULL,
               track_id INTEGER NOT NULL CHECK (track_id >= 0),
               kind TEXT NOT NULL CHECK (kind IN ('track_confirmed', 'track_lost')),
               label TEXT NOT NULL,
               confidence REAL NOT NULL CHECK (confidence >= 0 AND confidence <= 1),
               timestamp_ms INTEGER NOT NULL CHECK (timestamp_ms >= 0),
               x_min REAL NOT NULL,
               y_min REAL NOT NULL,
               x_max REAL NOT NULL,
               y_max REAL NOT NULL,
               CHECK (x_min >= 0 AND x_min <= x_max AND x_max <= 1),
               CHECK (y_min >= 0 AND y_min <= y_max AND y_max <= 1)
             );
             CREATE INDEX IF NOT EXISTS detection_events_camera_time
               ON detection_events(camera_id, timestamp_ms, id);
             CREATE INDEX IF NOT EXISTS detection_events_run
               ON detection_events(run_id, timestamp_ms, id);
             PRAGMA user_version = 1;",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, rusqlite::Error> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn count(&self) -> Result<u64, rusqlite::Error> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let count: i64 =
            connection.query_row("SELECT COUNT(*) FROM detection_events", [], |row| {
                row.get(0)
            })?;
        Ok(count.max(0) as u64)
    }

    pub fn integrity_check(&self) -> Result<Vec<String>, rusqlite::Error> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let mut statement = connection.prepare("PRAGMA integrity_check")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    pub fn list_recent(&self, limit: usize) -> Result<Vec<DetectionEvent>, rusqlite::Error> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let limit = i64::try_from(limit.min(10_000))
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let mut statement = connection.prepare(
            "SELECT run_id, camera_id, track_id, kind, label, confidence, timestamp_ms,
                    x_min, y_min, x_max, y_max
             FROM detection_events
             ORDER BY timestamp_ms DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], event_from_row)?;
        let mut events: Vec<_> = rows.collect::<Result<_, _>>()?;
        events.reverse();
        Ok(events)
    }
}

fn event_from_row(row: &rusqlite::Row<'_>) -> Result<DetectionEvent, rusqlite::Error> {
    let kind: String = row.get(3)?;
    let kind = match kind.as_str() {
        "track_confirmed" => DetectionEventKind::TrackConfirmed,
        "track_lost" => DetectionEventKind::TrackLost,
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                3,
                Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unknown detection event kind",
                )),
            ));
        }
    };
    Ok(DetectionEvent {
        run_id: row.get(0)?,
        camera_id: row.get(1)?,
        track_id: row.get::<_, i64>(2)?.max(0) as u64,
        kind,
        label: row.get(4)?,
        confidence: row.get::<_, f64>(5)? as f32,
        timestamp_ms: row.get::<_, i64>(6)?.max(0) as u64,
        bbox: crate::tracking::BoundingBox {
            x_min: row.get::<_, f64>(7)? as f32,
            y_min: row.get::<_, f64>(8)? as f32,
            x_max: row.get::<_, f64>(9)? as f32,
            y_max: row.get::<_, f64>(10)? as f32,
        },
    })
}

impl EventStore for SqliteEventStore {
    type Error = rusqlite::Error;

    fn save(&self, event: &DetectionEvent) -> Result<(), Self::Error> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let kind = match event.kind {
            crate::detection::DetectionEventKind::TrackConfirmed => "track_confirmed",
            crate::detection::DetectionEventKind::TrackLost => "track_lost",
        };
        let track_id = i64::try_from(event.track_id)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let timestamp_ms = i64::try_from(event.timestamp_ms)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        connection.execute(
            "INSERT INTO detection_events
               (run_id, camera_id, track_id, kind, label, confidence, timestamp_ms,
                x_min, y_min, x_max, y_max)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                event.run_id,
                event.camera_id,
                track_id,
                kind,
                event.label,
                event.confidence as f64,
                timestamp_ms,
                event.bbox.x_min as f64,
                event.bbox.y_min as f64,
                event.bbox.x_max as f64,
                event.bbox.y_max as f64,
            ],
        )?;
        Ok(())
    }

    fn list_after(
        &self,
        timestamp_ms: u64,
        limit: usize,
    ) -> Result<Vec<DetectionEvent>, Self::Error> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        let timestamp_ms = i64::try_from(timestamp_ms)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let limit = i64::try_from(limit.min(10_000))
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let mut statement = connection.prepare(
            "SELECT run_id, camera_id, track_id, kind, label, confidence, timestamp_ms,
                    x_min, y_min, x_max, y_max
             FROM detection_events
             WHERE timestamp_ms > ?1
             ORDER BY timestamp_ms, id
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![timestamp_ms, limit], event_from_row)?;
        rows.collect()
    }
}
