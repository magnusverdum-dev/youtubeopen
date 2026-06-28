use async_trait::async_trait;
use tokio::io::AsyncRead;

use super::types::{BackendError, HealthStatus, VideoInfo};

pub struct DownloadOutput {
    pub filename: String,
    pub content_type: String,
    pub reader: Box<dyn AsyncRead + Send + Sync + Unpin>,
    pub size_hint: Option<u64>,
}

#[async_trait]
pub trait VideoBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn backend_id(&self) -> &'static str;
    async fn health(&self) -> HealthStatus;
    async fn get_video_info(&self, url: &str) -> Result<VideoInfo, BackendError>;
    async fn download_video(
        &self,
        url: &str,
        format_id: &str,
        title: &str,
        merge_audio: bool,
    ) -> Result<DownloadOutput, BackendError>;
}
