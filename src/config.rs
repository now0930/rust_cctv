use std::{env, error::Error, path::PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceConfig {
    pub database_path: PathBuf,
    pub camera_id: String,
    pub rtsp_url: Option<String>,
    pub model_path: Option<PathBuf>,
    pub score_threshold: f32,
    pub inference_fps: f64,
}

impl ServiceConfig {
    pub fn from_env_and_args() -> Result<Option<Self>, Box<dyn Error>> {
        let mut database_path = env::var_os("RUST_CCTV_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("./data/events.sqlite3"));
        let mut camera_id =
            env::var("RUST_CCTV_CAMERA_ID").unwrap_or_else(|_| "logitech_c922".to_owned());
        let mut rtsp_url = env::var("RUST_CCTV_RTSP_URL").ok();
        let mut model_path = env::var_os("RUST_CCTV_MODEL_PATH").map(PathBuf::from);
        let mut score_threshold = env::var("RUST_CCTV_SCORE_THRESHOLD")
            .ok()
            .map(|value| value.parse::<f32>())
            .transpose()?
            .unwrap_or(0.25);
        let mut inference_fps = env::var("RUST_CCTV_INFERENCE_FPS")
            .ok()
            .map(|value| value.parse::<f64>())
            .transpose()?
            .unwrap_or(10.0);

        let mut args = env::args().skip(1);
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--help" | "-h" => return Ok(None),
                "--db-path" => {
                    let Some(value) = args.next() else {
                        return Err("--db-path requires a path".into());
                    };
                    database_path = PathBuf::from(value);
                }
                "--camera-id" => {
                    let Some(value) = args.next() else {
                        return Err("--camera-id requires an ID".into());
                    };
                    camera_id = value;
                }
                "--rtsp-url" => {
                    let Some(value) = args.next() else {
                        return Err("--rtsp-url requires a URL".into());
                    };
                    rtsp_url = Some(value);
                }
                "--model-path" => {
                    let Some(value) = args.next() else {
                        return Err("--model-path requires a path".into());
                    };
                    model_path = Some(PathBuf::from(value));
                }
                "--score-threshold" => {
                    let Some(value) = args.next() else {
                        return Err("--score-threshold requires a number".into());
                    };
                    score_threshold = value.parse()?;
                }
                "--inference-fps" => {
                    let Some(value) = args.next() else {
                        return Err("--inference-fps requires a number".into());
                    };
                    inference_fps = value.parse()?;
                }
                _ => return Err(format!("unknown argument: {argument}; use --help").into()),
            }
        }

        if database_path.as_os_str().is_empty() {
            return Err("database path must not be empty".into());
        }
        if camera_id.trim().is_empty() {
            return Err("camera ID must not be empty".into());
        }
        if let Some(url) = &rtsp_url {
            if !(url.starts_with("rtsp://") || url.starts_with("rtsps://")) {
                return Err("RTSP URL must start with rtsp:// or rtsps://".into());
            }
        }
        if !(0.0..=1.0).contains(&score_threshold) {
            return Err("score threshold must be between 0 and 1".into());
        }
        if !inference_fps.is_finite() || !(0.1..=30.0).contains(&inference_fps) {
            return Err("inference FPS must be between 0.1 and 30".into());
        }
        if model_path.is_some() && rtsp_url.is_none() {
            return Err("RTSP URL is required when a model path is configured".into());
        }

        Ok(Some(Self {
            database_path,
            camera_id,
            rtsp_url,
            model_path,
            score_threshold,
            inference_fps,
        }))
    }
}
