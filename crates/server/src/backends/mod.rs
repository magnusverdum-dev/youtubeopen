pub mod ytdlp;
pub mod purerust;

use crate::core::VideoBackend;

pub enum BackendType {
    YtDlp,
    PureRust,
}

impl BackendType {
    pub fn from_str(s: &str) -> Self {
        match s {
            "purerust" => BackendType::PureRust,
            _ => BackendType::YtDlp,
        }
    }

    pub fn backend<'a>(&self, ytdlp: &'a ytdlp::YtDlpBackend, purerust: &'a purerust::PureRustBackend) -> &'a dyn VideoBackend {
        match self {
            BackendType::YtDlp => ytdlp,
            BackendType::PureRust => purerust,
        }
    }
}
