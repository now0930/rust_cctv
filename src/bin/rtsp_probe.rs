use ffmpeg::{
    codec, format, frame, media,
    software::scaling::{context::Context as Scaler, flag::Flags},
};
use ffmpeg_next as ffmpeg;
use std::{
    env,
    error::Error,
    process,
    time::{Duration, Instant},
};

struct Metrics {
    started: Instant,
    last_report: Instant,
    frames: u64,
    motion_frames: u64,
    processing_total: Duration,
    processing_max: Duration,
}

impl Metrics {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last_report: now,
            frames: 0,
            motion_frames: 0,
            processing_total: Duration::ZERO,
            processing_max: Duration::ZERO,
        }
    }

    fn process_frame(
        &mut self,
        scaler: &mut Scaler,
        decoded: &frame::Video,
        scaled: &mut frame::Video,
        previous: &mut [u8],
        width: usize,
        height: usize,
    ) -> Result<(), ffmpeg::Error> {
        let started = Instant::now();
        scaler.run(decoded, scaled)?;

        let plane = scaled.data(0);
        let stride = scaled.stride(0);
        let mut total_difference = 0u64;
        let mut index = 0usize;

        for y in 0..height {
            let row = y * stride;
            for x in 0..width {
                let value = plane[row + x];
                if self.frames > 0 {
                    total_difference += value.abs_diff(previous[index]) as u64;
                }
                previous[index] = value;
                index += 1;
            }
        }

        if self.frames > 0 && total_difference / (width * height) as u64 > 5 {
            self.motion_frames += 1;
        }
        self.frames += 1;

        let elapsed = started.elapsed();
        self.processing_total += elapsed;
        self.processing_max = self.processing_max.max(elapsed);

        if self.last_report.elapsed() >= Duration::from_secs(5) {
            let wall_seconds = self.started.elapsed().as_secs_f64().max(0.001);
            let average_ms =
                self.processing_total.as_secs_f64() * 1000.0 / self.frames.max(1) as f64;
            eprintln!(
                "elapsed={wall_seconds:.1}s frames={} fps={:.1} motion_frames={} scale_diff_avg_ms={average_ms:.2} scale_diff_max_ms={:.2}",
                self.frames,
                self.frames as f64 / wall_seconds,
                self.motion_frames,
                self.processing_max.as_secs_f64() * 1000.0,
            );
            self.last_report = Instant::now();
        }

        Ok(())
    }
}

fn parse_arg<T: std::str::FromStr>(value: Option<String>, default: T, name: &str) -> T {
    match value {
        None => default,
        Some(value) => match value.parse() {
            Ok(parsed) => parsed,
            Err(_) => {
                eprintln!("invalid {name}");
                process::exit(2);
            }
        },
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let input_url = args
        .next()
        .ok_or("usage: rtsp-probe <rtsp-url> [seconds=30] [width=320] [height=180]")?;
    let seconds: u64 = parse_arg(args.next(), 30, "seconds");
    let width: u32 = parse_arg(args.next(), 320, "width");
    let height: u32 = parse_arg(args.next(), 180, "height");
    if seconds == 0 || width == 0 || height == 0 {
        return Err("seconds, width, and height must be greater than zero".into());
    }

    ffmpeg::init()?;
    let mut input = format::input(&input_url).map_err(|_| "failed to open RTSP input")?;
    let video_stream = input
        .streams()
        .best(media::Type::Video)
        .ok_or("no video stream found")?;
    let video_index = video_stream.index();
    let decoder_context = codec::context::Context::from_parameters(video_stream.parameters())?;
    let mut decoder = decoder_context.decoder().video()?;
    drop(video_stream);

    let mut scaler = Scaler::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        format::Pixel::GRAY8,
        width,
        height,
        Flags::BILINEAR,
    )?;
    let mut decoded = frame::Video::empty();
    let mut scaled = frame::Video::empty();
    let mut previous = vec![0u8; width as usize * height as usize];
    let mut metrics = Metrics::new();
    let duration = Duration::from_secs(seconds);

    eprintln!(
        "input={}x{} {:?}; analysis={}x{} gray8; duration={}s",
        decoder.width(),
        decoder.height(),
        decoder.format(),
        width,
        height,
        seconds
    );

    for (stream, packet) in input.packets() {
        if metrics.started.elapsed() >= duration {
            break;
        }
        if stream.index() != video_index {
            continue;
        }

        decoder.send_packet(&packet)?;
        while decoder.receive_frame(&mut decoded).is_ok() {
            metrics.process_frame(
                &mut scaler,
                &decoded,
                &mut scaled,
                &mut previous,
                width as usize,
                height as usize,
            )?;
        }
    }

    let elapsed = metrics.started.elapsed().as_secs_f64().max(0.001);
    let average_ms = metrics.processing_total.as_secs_f64() * 1000.0 / metrics.frames.max(1) as f64;
    eprintln!(
        "finished frames={} fps={:.1} motion_frames={} scale_diff_avg_ms={average_ms:.2} scale_diff_max_ms={:.2}",
        metrics.frames,
        metrics.frames as f64 / elapsed,
        metrics.motion_frames,
        metrics.processing_max.as_secs_f64() * 1000.0,
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("rust-cctv: {error}");
        process::exit(1);
    }
}
