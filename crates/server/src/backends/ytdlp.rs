use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::Command;

use crate::core::{
    BackendError, DownloadOutput, FormatEntry, HealthStatus, VideoBackend, VideoInfo,
};

pub struct YtDlpBackend;

fn sanitize_url(url: &str) -> bool {
    url.starts_with("https://www.youtube.com/")
        || url.starts_with("https://youtube.com/")
        || url.starts_with("https://youtu.be/")
        || url.starts_with("https://m.youtube.com/")
        || url.starts_with("https://www.yout-ube.com/")
        || url.starts_with("https://yout-ube.com/")
}

#[async_trait]
impl VideoBackend for YtDlpBackend {
    fn name(&self) -> &'static str {
        "yt-dlp + ffmpeg"
    }

    fn backend_id(&self) -> &'static str {
        "ytdlp"
    }

    async fn health(&self) -> HealthStatus {
        let ytdlp = Command::new("yt-dlp")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);

        let ffmpeg = Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);

        let available = ytdlp && ffmpeg;

        let version = if ytdlp {
            Command::new("yt-dlp")
                .arg("--version")
                .output()
                .await
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| {
                    let mut v = s.trim().to_string();
                    if ffmpeg {
                        v.push_str(" + ffmpeg");
                    }
                    v
                })
        } else {
            None
        };

        HealthStatus {
            backend_name: self.name().to_string(),
            available,
            version,
        }
    }

    async fn get_video_info(&self, url: &str) -> Result<VideoInfo, BackendError> {
        if !sanitize_url(url) {
            return Ok(VideoInfo {
                title: String::new(),
                duration: None,
                thumbnail: None,
                formats: vec![],
                error: Some("invalid youtube url".to_string()),
            });
        }

        let output = Command::new("yt-dlp")
            .args(["-J", "--no-playlist", "--flat-playlist", url])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| BackendError::ExtractionFailed(format!("failed to run yt-dlp: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Ok(VideoInfo {
                title: String::new(),
                duration: None,
                thumbnail: None,
                formats: vec![],
                error: Some(format!(
                    "yt-dlp error: {}",
                    stderr.trim().lines().last().unwrap_or("unknown")
                )),
            });
        }

        let raw = String::from_utf8(output.stdout)
            .map_err(|_| BackendError::ExtractionFailed("invalid utf-8 from yt-dlp".into()))?;

        let json: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| BackendError::ExtractionFailed(format!("parse error: {}", e)))?;

        let title = json["title"].as_str().unwrap_or("Unknown").to_string();
        let duration = json["duration"].as_f64();
        let thumbnail = json["thumbnail"].as_str().map(|s| s.to_string());

        let mut formats: Vec<FormatEntry> = vec![];

        if let Some(arr) = json["formats"].as_array() {
            for fmt in arr {
                let format_id = fmt["format_id"].as_str().unwrap_or("").to_string();
                let ext = fmt["ext"].as_str().unwrap_or("").to_string();
                let vcodec = fmt["vcodec"].as_str().unwrap_or("none").to_string();
                let acodec = fmt["acodec"].as_str().unwrap_or("none").to_string();

                let has_video = vcodec != "none";
                if !has_video {
                    continue;
                }

                let has_audio = acodec != "none";
                let height = fmt["height"].as_u64().unwrap_or(0);

                let resolution = if height > 0 {
                    if has_audio {
                        format!("{}p", height)
                    } else {
                        format!("{}p (video only)", height)
                    }
                } else {
                    "audio only".to_string()
                };

                let filesize = fmt["filesize"]
                    .as_u64()
                    .or_else(|| fmt["filesize_approx"].as_u64());

                let fps = fmt["fps"].as_f64();

                formats.push(FormatEntry {
                    format_id,
                    ext,
                    resolution,
                    filesize,
                    fps,
                    vcodec,
                    acodec,
                });
            }
        }

        formats.sort_by(|a, b| {
            let a_res = a
                .resolution
                .split('p')
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let b_res = b
                .resolution
                .split('p')
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            b_res.cmp(&a_res)
        });

        Ok(VideoInfo {
            title,
            duration,
            thumbnail,
            formats,
            error: None,
        })
    }

    async fn download_video(
        &self,
        url: &str,
        format_id: &str,
        _title: &str,
        merge_audio: bool,
    ) -> Result<DownloadOutput, BackendError> {
        if !sanitize_url(url) {
            return Err(BackendError::InvalidUrl("invalid youtube url".into()));
        }

        let video_id = extract_video_id_from_url(url).unwrap_or("unknown");
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let tmp_path = std::env::temp_dir().join(format!("yt_{}_{}_{}.mp4", video_id, format_id, ts));
        let tmp_str = tmp_path.to_string_lossy().to_string();

        let ffmpeg_dir = std::env::var("PATH")
            .ok()
            .and_then(|path_var| {
                path_var.split(';').find(|p| p.contains("ffmpeg")).map(|p| p.to_string())
            });

        let format_arg = if merge_audio {
            format!("{}+bestaudio[ext=m4a]/bestaudio/best", format_id)
        } else {
            format_id.to_string()
        };

        let mut cmd = Command::new("yt-dlp");
        cmd.args([
            "-f",
            &format_arg,
            "--merge-output-format",
            "mp4",
            "--no-playlist",
            "-o",
            &tmp_str,
            "--no-part",
            "--no-mtime",
            "--no-progress",
            "-q",
        ]);

        if let Some(ref dir) = ffmpeg_dir {
            cmd.arg("--ffmpeg-location").arg(dir);
        }

        let output = cmd
            .arg(url)
            .output()
            .await
            .map_err(|e| BackendError::DownloadFailed(format!("failed to run yt-dlp: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(BackendError::DownloadFailed(format!(
                "yt-dlp failed: {}",
                stderr.trim().lines().last().unwrap_or("unknown")
            )));
        }

        let path = &tmp_path;
        if !path.exists() {
            return Err(BackendError::DownloadFailed(format!(
                "yt-dlp output file not found: {}",
                tmp_str
            )));
        }

        let file = tokio::fs::File::open(path)
            .await
            .map_err(|e| BackendError::DownloadFailed(format!("failed to open yt-dlp output: {}", e)))?;
        let size = file.metadata().await.map(|m| m.len()).ok();

        let stream = tokio_util::io::ReaderStream::new(file);

        let path_clone = path.to_path_buf();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let _ = tokio::fs::remove_file(&path_clone).await;
        });

        Ok(DownloadOutput {
            filename: format!("{}.mp4", _title.trim()),
            content_type: "video/mp4".to_string(),
            reader: Box::new(tokio_util::io::StreamReader::new(stream)),
            size_hint: size,
        })
    }
}

fn extract_video_id_from_url(url: &str) -> Option<&str> {
    if let Some(rest) = url.strip_prefix("https://www.youtube.com/watch?v=") {
        return rest.split('&').next();
    }
    if let Some(rest) = url.strip_prefix("https://youtube.com/watch?v=") {
        return rest.split('&').next();
    }
    if let Some(rest) = url.strip_prefix("https://m.youtube.com/watch?v=") {
        return rest.split('&').next();
    }
    if let Some(rest) = url.strip_prefix("https://youtu.be/") {
        return rest.split('?').next();
    }
    None
}
