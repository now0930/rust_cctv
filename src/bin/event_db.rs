use rust_cctv::{detection::DetectionEventKind, storage::SqliteEventStore};
use std::{env, error::Error, path::Path};

fn main() {
    if let Err(error) = run() {
        eprintln!("event-db: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let Some(path) = args.next() else {
        return Err("usage: event-db <database-path> [recent-limit]".into());
    };
    if !Path::new(&path).is_file() {
        return Err(format!("database file does not exist: {path}").into());
    }
    let limit = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(20)
        .min(10_000);
    if args.next().is_some() {
        return Err("usage: event-db <database-path> [recent-limit]".into());
    }

    let store = SqliteEventStore::open_read_only(path)?;
    let integrity = store.integrity_check()?;
    println!("integrity={}", integrity.join("; "));
    println!("events_total={}", store.count()?);

    let events = store.list_recent(limit)?;
    println!("events_listed={}", events.len());
    for event in events {
        let kind = match event.kind {
            DetectionEventKind::TrackConfirmed => "track_confirmed",
            DetectionEventKind::TrackLost => "track_lost",
        };
        println!(
            "timestamp_ms={} camera={} track={} kind={} label={} confidence={:.3} bbox=({:.3},{:.3},{:.3},{:.3}) run={}",
            event.timestamp_ms,
            event.camera_id,
            event.track_id,
            kind,
            event.label,
            event.confidence,
            event.bbox.x_min,
            event.bbox.y_min,
            event.bbox.x_max,
            event.bbox.y_max,
            event.run_id,
        );
    }
    Ok(())
}
