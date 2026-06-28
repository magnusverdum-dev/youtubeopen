use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatEntry {
    pub format_id: String,
    pub ext: String,
    pub resolution: String,
    pub filesize: Option<u64>,
    pub fps: Option<f64>,
    pub vcodec: String,
    pub acodec: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub title: String,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub formats: Vec<FormatEntry>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub backend_name: String,
    pub available: bool,
    pub version: Option<String>,
}

#[derive(Debug)]
pub enum BackendError {
    NotFound(String),
    InvalidUrl(String),
    ExtractionFailed(String),
    DownloadFailed(String),
    Io(std::io::Error),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::NotFound(msg) => write!(f, "not found: {}", msg),
            BackendError::InvalidUrl(msg) => write!(f, "invalid url: {}", msg),
            BackendError::ExtractionFailed(msg) => write!(f, "extraction failed: {}", msg),
            BackendError::DownloadFailed(msg) => write!(f, "download failed: {}", msg),
            BackendError::Io(e) => write!(f, "io error: {}", e),
        }
    }
}

impl From<std::io::Error> for BackendError {
    fn from(e: std::io::Error) -> Self {
        BackendError::Io(e)
    }
}
