use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

use crate::core::{
    BackendError, DownloadOutput, FormatEntry, HealthStatus, VideoBackend, VideoInfo,
};

macro_rules! log_debug {
    ($($arg:tt)*) => {{
        use std::fs::OpenOptions;
        use std::io::Write;
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open("purerust_debug.log") {
            let _ = writeln!(f, "[{}] {}", chrono::Local::now().format("%H:%M:%S%.3f"), format!($($arg)*));
        }
    }};
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InnertubePlayer {
    playability_status: InnertubePlayability,
    streaming_data: Option<InnertubeStreamingData>,
    video_details: Option<InnertubeVideoDetails>,
    response_context: Option<InnertubeResponseContext>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "SCREAMING_SNAKE_CASE")]
enum InnertubePlayability {
    Ok {},
    #[serde(rename_all = "camelCase")]
    Unplayable { reason: Option<String> },
    #[serde(rename_all = "camelCase")]
    LoginRequired { reason: Option<String> },
    #[serde(rename_all = "camelCase")]
    Error { reason: Option<String> },
    #[serde(rename_all = "camelCase")]
    LiveStreamOffline { reason: Option<String> },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InnertubeStreamingData {
    expires_in_seconds: Option<String>,
    formats: Option<Vec<InnertubeFormat>>,
    adaptive_formats: Option<Vec<InnertubeFormat>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InnertubeFormat {
    itag: u32,
    url: Option<String>,
    #[serde(rename = "type")]
    format_type: Option<String>,
    mime_type: String,
    bitrate: u32,
    width: Option<u32>,
    height: Option<u32>,
    quality: Option<String>,
    quality_label: Option<String>,
    fps: Option<u32>,
    content_length: Option<String>,
    audio_quality: Option<String>,
    audio_sample_rate: Option<String>,
    audio_channels: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InnertubeVideoDetails {
    video_id: String,
    title: Option<String>,
    length_seconds: Option<String>,
    thumbnail: Option<InnertubeThumbnail>,
    channel_id: Option<String>,
    author: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InnertubeThumbnail {
    thumbnails: Vec<InnertubeThumbnailItem>,
}

#[derive(Debug, Deserialize)]
struct InnertubeThumbnailItem {
    url: String,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct InnertubeResponseContext {
    visitor_data: Option<String>,
}

const CONSENT_COOKIE: &str = "SOCS=CAISAiAD";
const YTMUSIC_URL: &str = "https://music.youtube.com";
const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

struct CookieJar {
    cookie: RwLock<Option<(String, Instant)>>,
    http: reqwest::Client,
}

impl CookieJar {
    fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(CHROME_UA)
            .build()
            .expect("Failed to build cookie-fetching client");
        Self {
            cookie: RwLock::new(None),
            http,
        }
    }

    async fn get_cookie(&self) -> Result<String, BackendError> {
        {
            let lock = self.cookie.read().await;
            if let Some((ref c, ts)) = *lock {
                if ts.elapsed() < Duration::from_secs(1800) {
                    return Ok(c.clone());
                }
            }
        }

        let resp = self
            .http
            .get(YTMUSIC_URL)
            .header(reqwest::header::ORIGIN, YTMUSIC_URL)
            .header(reqwest::header::REFERER, YTMUSIC_URL)
            .header(reqwest::header::COOKIE, CONSENT_COOKIE)
            .send()
            .await
            .map_err(|e| BackendError::DownloadFailed(format!("cookie fetch failed: {}", e)))?;

        let mut yec_cookie = String::new();
        let mut all_cookies: Vec<String> = Vec::new();

        for val in resp.headers().get_all("set-cookie").iter() {
            if let Ok(s) = val.to_str() {
                if let Some(name_eq_val) = s.split(';').next() {
                    all_cookies.push(name_eq_val.trim().to_string());
                    if name_eq_val.starts_with("__Secure-YEC=") {
                        yec_cookie = name_eq_val.trim().to_string();
                    }
                }
            }
        }

        if yec_cookie.is_empty() {
            tracing::warn!("[purerust] no __Secure-YEC cookie found, using consent only");
            yec_cookie = CONSENT_COOKIE.to_string();
        }

        let combined = if all_cookies.len() > 1 {
            all_cookies.join("; ")
        } else {
            format!("{}; {}", CONSENT_COOKIE, yec_cookie)
        };

        tracing::info!("[purerust] fetched cookies: {}", truncate_str(&combined, 120));

        let mut lock = self.cookie.write().await;
        *lock = Some((combined.clone(), Instant::now()));

        Ok(combined)
    }
}

pub struct PureRustBackend {
    rp: Arc<rustypipe::client::RustyPipe>,
    client: reqwest::Client,
    cookies: Arc<CookieJar>,
    botguard_path: Option<std::path::PathBuf>,
}

impl PureRustBackend {
    pub fn new() -> Self {
        let botguard_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("rustypipe-botguard.exe")))
            .filter(|p| p.exists())
            .or_else(|| {
                let local = std::path::PathBuf::from("rustypipe-botguard.exe");
                if local.exists() { Some(local) } else { None }
            });

        let rp_builder = rustypipe::client::RustyPipe::builder()
            .storage_dir("./rustypipe_cache");

        let rp_builder = if let Some(path) = &botguard_path {
            eprintln!("[purerust] using botguard at: {}", path.display());
            rp_builder.botguard_bin(path.clone()).po_token_cache()
        } else {
            eprintln!("[purerust] rustypipe-botguard NOT found, PO tokens disabled");
            rp_builder
        };

        let rp = rp_builder
            .build()
            .expect("failed to build RustyPipe client");

        let client = reqwest::Client::builder()
            .user_agent(CHROME_UA)
            .gzip(true)
            .brotli(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build reqwest Client");

        Self {
            rp: Arc::new(rp),
            client,
            cookies: Arc::new(CookieJar::new()),
            botguard_path,
        }
    }

    pub async fn get_player_debug(&self, video_id: &str) -> Result<rustypipe::model::VideoPlayer, rustypipe::error::Error> {
        self.try_player(video_id).await
    }

    async fn try_player(&self, video_id: &str) -> Result<rustypipe::model::VideoPlayer, rustypipe::error::Error> {
        let mut last_err = None;

        for client in &[rustypipe::client::ClientType::Desktop, rustypipe::client::ClientType::Android, rustypipe::client::ClientType::Ios, rustypipe::client::ClientType::Tv] {
            match self.rp.query().player_from_clients(video_id, &[*client]).await {
                Ok(mut player) => {
                    if player.video_only_streams.is_empty() && player.video_streams.is_empty() && player.audio_streams.is_empty() {
                        eprintln!("[purerust] client {:?} returned no streams, trying next", client);
                        last_err = Some(rustypipe::error::Error::Other(format!("{:?} returned no streams", client).into()));
                        continue;
                    }
                    let urls_have_pot = player.video_only_streams.iter().any(|s| s.url.contains("pot="))
                        || player.video_streams.iter().any(|s| s.url.contains("pot="))
                        || player.audio_streams.iter().any(|s| s.url.contains("pot="));
                    if !urls_have_pot {
                        self.inject_pot_into_player(&mut player, video_id).await;
                    }
                    return Ok(player);
                }
                Err(e) => {
                    eprintln!("[purerust] client {:?} failed: {}", client, e);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or(rustypipe::error::Error::Other("no clients available".into())))
    }

    async fn raw_player_android(&self, video_id: &str) -> Result<InnertubePlayer, BackendError> {
        let visitor_data = match self.rp.query().get_visitor_data(false).await {
            Ok(vd) => vd,
            Err(e) => {
                eprintln!("[purerust] failed to get visitor data: {}", e);
                String::new()
            }
        };

        let mut body = serde_json::json!({
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
        });

        if !visitor_data.is_empty() {
            if let Some(pot) = self.generate_po_token_safe(&visitor_data, video_id) {
                body["serviceIntegrityDimensions"] = serde_json::json!({
                    "poToken": pot
                });
                eprintln!("[purerust] injected PO token into raw Android request for {}", video_id);
            } else {
                eprintln!("[purerust] failed to generate PO token for Android request");
            }
        }

        let response_str = match self.try_raw_client(rustypipe::client::ClientType::Android, &body).await {
            Ok(s) => s,
            Err(android_err) => {
                eprintln!("[purerust] raw Android failed ({}), falling back to iOS", android_err);
                self.try_raw_client(rustypipe::client::ClientType::Ios, &body).await
                    .map_err(|ios_err| {
                        eprintln!("[purerust] raw iOS fallback also failed: {}", ios_err);
                        ios_err
                    })?
            }
        };
        let player: InnertubePlayer = match serde_json::from_str(&response_str) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[purerust] raw parse failed: {}, body preview: {}", e, truncate_str(&response_str, 300));
                return Err(BackendError::ExtractionFailed(format!("parse player: {}", e)));
            }
        };
        Ok(player)
    }

    async fn try_raw_client(&self, client: rustypipe::client::ClientType, body: &serde_json::Value) -> Result<String, BackendError> {
        self.rp.query()
            .raw(client, "player", body)
            .await
            .map_err(|e| BackendError::ExtractionFailed(format!("{} raw player error: {}", format!("{:?}", client), e)))
    }

    async fn raw_player_ios_with_po_token(&self, video_id: &str) -> Result<InnertubePlayer, BackendError> {
        let visitor_data = match self.rp.query().get_visitor_data(false).await {
            Ok(vd) => vd,
            Err(e) => {
                eprintln!("[purerust] failed to get visitor data: {}", e);
                String::new()
            }
        };

        let mut body = serde_json::json!({
            "videoId": video_id,
            "contentCheckOk": true,
            "racyCheckOk": true,
        });

        if !visitor_data.is_empty() {
            if let Some(pot) = self.generate_po_token_safe(&visitor_data, video_id) {
                body["serviceIntegrityDimensions"] = serde_json::json!({
                    "poToken": pot
                });
                eprintln!("[purerust] injected PO token into raw IOS request for {}", video_id);
            } else {
                eprintln!("[purerust] failed to generate PO token for IOS request");
            }
        }

        let response_str = self.rp.query()
            .raw(rustypipe::client::ClientType::Ios, "player", &body)
            .await
            .map_err(|e| BackendError::ExtractionFailed(format!("raw player: {}", e)))?;
        let player: InnertubePlayer = serde_json::from_str(&response_str)
            .map_err(|e| BackendError::ExtractionFailed(format!("parse player: {}", e)))?;
        Ok(player)
    }

    fn generate_po_token_safe(&self, visitor_data: &str, video_id: &str) -> Option<String> {
        let path = self.botguard_path.as_ref()?;
        let snapshot_file = std::path::PathBuf::from("./rustypipe_cache/bg_snapshot.bin");
        
        let mut cmd = std::process::Command::new(path);
        if snapshot_file.exists() {
            cmd.arg("--snapshot-file").arg(&snapshot_file);
        }
        cmd.arg("--").arg(visitor_data).arg(video_id);
        
        match cmd.output() {
            Ok(output) => {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let token = stdout.split_whitespace().next()?.to_string();
                    if !token.is_empty() {
                        eprintln!("[purerust] generated PO token via safe method (len={})", token.len());
                        return Some(token);
                    }
                } else {
                    eprintln!("[purerust] botguard failed with status: {:?}, stderr: {}", output.status, String::from_utf8_lossy(&output.stderr));
                }
            }
            Err(e) => {
                eprintln!("[purerust] botguard execution error: {}", e);
            }
        }
        None
    }

    fn innertube_to_format(player: &InnertubePlayer) -> Vec<FormatEntry> {
        let mut formats = Vec::new();
        if let Some(ref sd) = player.streaming_data {
            let all_formats = sd.formats.iter().flatten()
                .chain(sd.adaptive_formats.iter().flatten());
            for f in all_formats {
                let is_audio = f.audio_quality.is_some();
                let resolution = if is_audio {
                    format!("{}kbps (audio only)", f.bitrate / 1000)
                } else if let Some(ref label) = f.quality_label {
                    if let Some(fps) = f.fps {
                        format!("{} (video only)", label.replace("p", &format!("p{}", fps)).replace("p0p", "p"))
                    } else {
                        format!("{} (video only)", label)
                    }
                } else {
                    format!("{}p", f.height.unwrap_or(0))
                };
                let ext = if f.mime_type.contains("webm") { "webm" }
                    else if f.mime_type.contains("mp4") { "mp4" }
                    else if f.mime_type.contains("3gpp") { "3gp" }
                    else { "mp4" };
                let vcodec = if is_audio { "none".to_string() }
                    else if f.mime_type.contains("avc1") { "avc1".to_string() }
                    else if f.mime_type.contains("vp9") { "vp9".to_string() }
                    else if f.mime_type.contains("av01") { "av01".to_string() }
                    else { "unknown".to_string() };
                let acodec = if is_audio {
                    if f.mime_type.contains("mp4a") || f.mime_type.contains("aac") { "mp4a".to_string() }
                    else if f.mime_type.contains("opus") { "opus".to_string() }
                    else { "unknown".to_string() }
                } else { "none".to_string() };
                formats.push(FormatEntry {
                    format_id: f.itag.to_string(),
                    ext: ext.to_string(),
                    resolution,
                    filesize: f.content_length.as_deref().and_then(|s| s.parse().ok()),
                    fps: f.fps.map(|f| f as f64),
                    vcodec,
                    acodec,
                });
            }
        }
        formats
    }

    async fn inject_pot_into_player(&self, player: &mut rustypipe::model::VideoPlayer, video_id: &str) {
        let visitor_data = if let Some(ref vd) = player.visitor_data {
            if !vd.is_empty() {
                vd.clone()
            } else {
                match self.rp.query().get_visitor_data(false).await {
                    Ok(vd) => vd,
                    Err(e) => {
                        eprintln!("[purerust] failed to get visitor data: {}", e);
                        return;
                    }
                }
            }
        } else {
            match self.rp.query().get_visitor_data(false).await {
                Ok(vd) => vd,
                Err(e) => {
                    eprintln!("[purerust] failed to get visitor data: {}", e);
                    return;
                }
            }
        };
        if visitor_data.is_empty() {
            eprintln!("[purerust] no visitor data for PO token generation");
            return;
        }
        let Some(pot) = self.generate_po_token(&visitor_data, video_id) else {
            return;
        };
        for s in &mut player.video_streams {
            s.url = Self::inject_pot(&s.url, &pot);
        }
        for s in &mut player.video_only_streams {
            s.url = Self::inject_pot(&s.url, &pot);
        }
        for s in &mut player.audio_streams {
            s.url = Self::inject_pot(&s.url, &pot);
        }
        player.visitor_data = Some(visitor_data);
        eprintln!("[purerust] injected PO token into all stream URLs for {}", video_id);
    }

    fn generate_po_token(&self, visitor_data: &str, video_id: &str) -> Option<String> {
        let path = self.botguard_path.as_ref()?;
        let snapshot_file = std::path::PathBuf::from("./rustypipe_cache/bg_snapshot.bin");
        let mut cmd = std::process::Command::new(path);
        if snapshot_file.exists() {
            cmd.arg("--snapshot-file").arg(&snapshot_file);
        }
        cmd.arg("--").arg(visitor_data);
        let output = cmd.output().ok()?;
        if !output.status.success() {
            eprintln!("[purerust] botguard failed: {}", String::from_utf8_lossy(&output.stderr));
            return None;
        }
        let stdout = String::from_utf8(output.stdout).ok()?;
        let session_token = stdout.split_whitespace().next()?.to_string();
        eprintln!("[purerust] generated session PO token for {} (len={})", video_id, session_token.len());
        Some(session_token)
    }

    fn inject_pot(url: &str, pot: &str) -> String {
        if url.contains("pot=") {
            return url.to_string();
        }
        let separator = if url.contains('?') { '&' } else { '?' };
        format!("{}{}pot={}", url, separator, pot)
    }
}

impl Default for PureRustBackend {
    fn default() -> Self {
        Self::new()
    }
}

pub fn extract_video_id(url: &str) -> Option<String> {
    let url = url.trim();
    if let Some(rest) = url.strip_prefix("https://www.youtube.com/watch?v=") {
        return rest.split('&').next().map(|s| s.to_string());
    }
    if let Some(rest) = url.strip_prefix("https://youtube.com/watch?v=") {
        return rest.split('&').next().map(|s| s.to_string());
    }
    if let Some(rest) = url.strip_prefix("https://m.youtube.com/watch?v=") {
        return rest.split('&').next().map(|s| s.to_string());
    }
    if let Some(rest) = url.strip_prefix("https://youtu.be/") {
        return rest.split('?').next().map(|s| s.to_string());
    }
    if let Some(rest) = url.strip_prefix("https://www.yout-ube.com/watch?v=") {
        return rest.split('&').next().map(|s| s.to_string());
    }
    if let Some(rest) = url.strip_prefix("https://yout-ube.com/watch?v=") {
        return rest.split('&').next().map(|s| s.to_string());
    }
    None
}

#[async_trait]
impl VideoBackend for PureRustBackend {
    fn name(&self) -> &'static str {
        "Pure Rust (rustypipe)"
    }

    fn backend_id(&self) -> &'static str {
        "purerust"
    }

    async fn health(&self) -> HealthStatus {
        HealthStatus {
            backend_name: self.name().to_string(),
            available: true,
            version: Some(format!("rustypipe {}", rustypipe::VERSION)),
        }
    }

    async fn get_video_info(&self, url: &str) -> Result<VideoInfo, BackendError> {
        let video_id = match extract_video_id(url) {
            Some(id) => id,
            None => {
                return Ok(VideoInfo {
                    title: String::new(),
                    duration: None,
                    thumbnail: None,
                    formats: vec![],
                    error: Some("invalid youtube url".to_string()),
                });
            }
        };

        match self.raw_player_ios_with_po_token(&video_id).await {
            Ok(raw_player) => {
                let title = raw_player.video_details.as_ref()
                    .and_then(|d| d.title.clone())
                    .unwrap_or_default();
                let duration = raw_player.video_details.as_ref()
                    .and_then(|d| d.length_seconds.as_deref())
                    .and_then(|s| s.parse::<f64>().ok());
                let thumbnail = raw_player.video_details.as_ref()
                    .and_then(|d| d.thumbnail.as_ref())
                    .and_then(|t| t.thumbnails.iter().max_by_key(|t| t.width.unwrap_or(0) * t.height.unwrap_or(0)))
                    .map(|t| t.url.clone());
                let mut formats = Self::innertube_to_format(&raw_player);
                formats.sort_by(|a, b| {
                    let a_res = parse_height(&a.resolution).unwrap_or(0);
                    let b_res = parse_height(&b.resolution).unwrap_or(0);
                    b_res.cmp(&a_res)
                });
                eprintln!("[purerust] raw IOS with PO token: {} formats for {}", formats.len(), video_id);
                return Ok(VideoInfo { title, duration, thumbnail, formats, error: None });
            }
            Err(e) => {
                eprintln!("[purerust] raw IOS with PO token failed: {}, falling back to try_player", e);
            }
        }

        let player = self
            .try_player(&video_id)
            .await
            .map_err(|e| BackendError::ExtractionFailed(format!("player: {}", e)))?;

        let title = player.details.name.clone().unwrap_or_default();
        let duration = Some(player.details.duration as f64);
        let thumbnail = player
            .details
            .thumbnail
            .iter()
            .max_by_key(|t| t.width * t.height)
            .map(|t| t.url.clone());

        let mut formats: Vec<FormatEntry> = vec![];

        for s in &player.video_streams {
            formats.push(FormatEntry {
                format_id: s.itag.to_string(),
                ext: stream_format_to_ext(&s.mime, &s.quality),
                resolution: if s.fps > 0 {
                    format!("{}p{}", s.height, s.fps)
                } else {
                    format!("{}p", s.height)
                },
                filesize: s.size,
                fps: Some(s.fps as f64),
                vcodec: format!("{:?}", s.codec).to_lowercase(),
                acodec: "aac".to_string(),
            });
        }

        for s in &player.video_only_streams {
            formats.push(FormatEntry {
                format_id: s.itag.to_string(),
                ext: stream_format_to_ext(&s.mime, &s.quality),
                resolution: if s.fps > 0 {
                    format!("{}p{} (video only)", s.height, s.fps)
                } else {
                    format!("{}p (video only)", s.height)
                },
                filesize: s.size,
                fps: Some(s.fps as f64),
                vcodec: format!("{:?}", s.codec).to_lowercase(),
                acodec: "none".to_string(),
            });
        }

        for s in &player.audio_streams {
            formats.push(FormatEntry {
                format_id: s.itag.to_string(),
                ext: stream_format_to_ext(&s.mime, ""),
                resolution: format!("{}kbps (audio only)", s.bitrate / 1000),
                filesize: Some(s.size),
                fps: None,
                vcodec: "none".to_string(),
                acodec: format!("{:?}", s.codec).to_lowercase(),
            });
        }

        formats.sort_by(|a, b| {
            let a_res = parse_height(&a.resolution).unwrap_or(0);
            let b_res = parse_height(&b.resolution).unwrap_or(0);
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
        title: &str,
        merge_audio: bool,
    ) -> Result<DownloadOutput, BackendError> {
        let video_id = extract_video_id(url)
            .ok_or_else(|| BackendError::InvalidUrl("invalid youtube url".into()))?;
        let itag: u32 = format_id
            .parse()
            .map_err(|_| BackendError::InvalidUrl(format!("invalid format_id: {}", format_id)))?;

        log_debug!("[purerust] download_video start: video_id={}, format_id={}, merge_audio={}", video_id, itag, merge_audio);

        if let Ok(raw_player) = self.raw_player_ios_with_po_token(&video_id).await {
            log_debug!("[purerust] raw IOS+PO succeeded");
            let all_formats: Vec<&InnertubeFormat> = raw_player.streaming_data.as_ref()
                .into_iter()
                .flat_map(|sd| {
                    sd.formats.iter().flatten()
                        .chain(sd.adaptive_formats.iter().flatten())
                })
                .collect();
            let mut visitor_data = raw_player.response_context.as_ref()
                .and_then(|rc| rc.visitor_data.clone())
                .unwrap_or_default();
            if visitor_data.is_empty() {
                visitor_data = self.rp.query().get_visitor_data(false).await.unwrap_or_default();
            }
            let cookie_str = self.cookies.get_cookie().await?;

            if let Some(f) = all_formats.iter().find(|f| f.itag == itag) {
                let mut stream_url = f.url.as_deref().ok_or_else(|| BackendError::DownloadFailed("format has no URL".into()))?.to_string();
                let is_audio = f.audio_quality.is_some();
                let mime = &f.mime_type;

                log_debug!("[purerust] raw IOS+PO: stream_url has_pot={}, visitor_data_len={}", stream_url.contains("pot="), visitor_data.len());
                if !stream_url.contains("pot=") && !visitor_data.is_empty() {
                    if let Some(pot) = self.generate_po_token_safe(&visitor_data, &video_id) {
                        log_debug!("[purerust] injected PO token into stream URL (len={})", pot.len());
                        stream_url = Self::inject_pot(&stream_url, &pot);
                    } else {
                        log_debug!("[purerust] generate_po_token_safe returned None for stream");
                    }
                }
                log_debug!("[purerust] final stream_url has_pot={}", stream_url.contains("pot="));

                if is_audio || !merge_audio {
                    return stream_direct_download(&self.client, stream_url, title, mime, &visitor_data, &cookie_str).await;
                }

                let video_is_mp4 = mime.contains("mp4");
                let best_audio = all_formats.iter()
                    .filter(|a| a.audio_quality.is_some() && if video_is_mp4 { a.mime_type.contains("mp4") } else { a.mime_type.contains("webm") })
                    .max_by_key(|a| a.bitrate)
                    .or_else(|| all_formats.iter().filter(|a| a.audio_quality.is_some()).max_by_key(|a| a.bitrate))
                    .ok_or_else(|| BackendError::DownloadFailed("no audio for merge".into()))?;
                let mut audio_url = best_audio.url.as_deref().ok_or_else(|| BackendError::DownloadFailed("audio has no URL".into()))?.to_string();
                
                if !audio_url.contains("pot=") && !visitor_data.is_empty() {
                    if let Some(pot) = self.generate_po_token_safe(&visitor_data, &video_id) {
                        audio_url = Self::inject_pot(&audio_url, &pot);
                    }
                }
                log_debug!("[purerust] final audio_url has_pot={}", audio_url.contains("pot="));

                return merge_streams(&self.client, stream_url, audio_url, title, mime, &visitor_data, &cookie_str).await;
            }
        } else {
            log_debug!("[purerust] raw IOS+PO failed, falling back to try_player");
        }

        let player = self
            .try_player(&video_id)
            .await
            .map_err(|e| BackendError::DownloadFailed(format!("player: {}", e)))?;

        let visitor_data = player.visitor_data.clone().unwrap_or_default();
        let cookie_str = self.cookies.get_cookie().await?;

        if let Some(vs) = player.video_streams.iter().find(|s| s.itag == itag) {
            tracing::info!("[purerust] streaming combined format {} ({}p) url_has_pot={}", itag, vs.height, vs.url.contains("pot="));
            return stream_direct_download(&self.client, vs.url.clone(), title, &vs.mime, &visitor_data, &cookie_str).await;
        }

        if let Some(vo) = player.video_only_streams.iter().find(|s| s.itag == itag) {
            if !merge_audio {
                tracing::info!("[purerust] streaming video_only {} ({}p, {}) url_has_pot={}", itag, vo.height, vo.mime, vo.url.contains("pot="));
                return stream_direct_download(&self.client, vo.url.clone(), title, &vo.mime, &visitor_data, &cookie_str).await;
            }

            let video_is_mp4 = vo.mime.contains("mp4");
            let best_audio = player.audio_streams.iter()
                .filter(|a| if video_is_mp4 { a.mime.contains("mp4") } else { a.mime.contains("webm") })
                .max_by_key(|a| a.bitrate)
                .or_else(|| player.audio_streams.iter().max_by_key(|a| a.bitrate))
                .ok_or_else(|| BackendError::DownloadFailed("no audio for merge".into()))?;

            tracing::info!("[purerust] merging video_only {} + audio {}", itag, best_audio.itag);
            return merge_streams(&self.client, vo.url.clone(), best_audio.url.clone(), title, &vo.mime, &visitor_data, &cookie_str).await;
        }

        if let Some(a) = player.audio_streams.iter().find(|s| s.itag == itag) {
            tracing::info!("[purerust] streaming audio only {} ({}kbps) url_has_pot={}", itag, a.bitrate / 1000, a.url.contains("pot="));
            return stream_direct_download(&self.client, a.url.clone(), title, &a.mime, &visitor_data, &cookie_str).await;
        }

        Err(BackendError::NotFound(format!(
            "format_id {} not found in player response",
            format_id
        )))
    }
}

fn stream_format_to_ext(mime: &str, _quality: &str) -> String {
    if mime.contains("mp4") {
        "mp4".to_string()
    } else if mime.contains("webm") {
        "webm".to_string()
    } else {
        "mp4".to_string()
    }
}

fn parse_height(res: &str) -> Option<u64> {
    res.split('p').next()?.parse().ok()
}

fn ext_for_mime(mime: &str) -> &'static str {
    if mime.contains("webm") {
        "webm"
    } else {
        "mp4"
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

fn build_cdn_headers<'a>(
    visitor_data: &'a str,
    cookie: &'a str,
) -> Vec<(&'a str, &'a str)> {
    vec![
        (reqwest::header::RANGE.as_str(), "bytes=0-"),
        (reqwest::header::ORIGIN.as_str(), "https://www.youtube.com"),
        (reqwest::header::REFERER.as_str(), "https://www.youtube.com/"),
        ("X-Goog-Visitor-Id", visitor_data),
        (reqwest::header::COOKIE.as_str(), cookie),
    ]
}

async fn stream_direct_download(
    client: &reqwest::Client,
    url: String,
    title: &str,
    mime: &str,
    visitor_data: &str,
    cookie: &str,
) -> Result<DownloadOutput, BackendError> {
    let ext = ext_for_mime(mime);
    let total = fetch_with_retry(client, &url, visitor_data, cookie).await?;

    let mut req = client.get(&url);
    for (k, v) in build_cdn_headers(visitor_data, cookie) {
        req = req.header(k, v);
    }

    let stream = req
        .send()
        .await
        .map_err(|e| BackendError::DownloadFailed(format!("failed to fetch stream: {}", e)))?
        .bytes_stream()
        .map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
    let reader = tokio_util::io::StreamReader::new(stream);

    Ok(DownloadOutput {
        filename: format!("{}.{}", title.trim(), ext),
        content_type: format!("video/{}", ext),
        reader: Box::new(reader),
        size_hint: total,
    })
}

async fn fetch_with_retry(
    client: &reqwest::Client,
    url: &str,
    visitor_data: &str,
    cookie: &str,
) -> Result<Option<u64>, BackendError> {
    let mut last_err = String::new();
    for attempt in 0..3u32 {
        let mut req = client.get(url);
        for (k, v) in build_cdn_headers(visitor_data, cookie) {
            req = req.header(k, v);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("network: {}", e);
                tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
                continue;
            }
        };

        let status = resp.status();
        if status.is_success() || status == reqwest::StatusCode::PARTIAL_CONTENT {
            let len = resp.content_length();
            tracing::info!(
                "[purerust] stream ok status={} content_length={:?}",
                status,
                len,
            );
            return Ok(len);
        }

        let body_preview = resp
            .text()
            .await
            .unwrap_or_default();
        last_err = format!("HTTP {} - {}", status, truncate_str(&body_preview, 200));
        tracing::warn!(
            "[purerust] attempt {} failed status={} body={}",
            attempt + 1,
            status,
            truncate_str(&body_preview, 300)
        );
        tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
    }

    Err(BackendError::DownloadFailed(format!(
        "stream unreachable after retries: {}",
        last_err
    )))
}

fn find_box(data: &[u8], target: &[u8; 4]) -> Option<(usize, usize)> {
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        if size < 8 {
            return None;
        }
        if &data[pos + 4..pos + 8] == target {
            return Some((pos, size));
        }
        pos += size;
    }
    None
}

fn find_init_size(data: &[u8]) -> Option<usize> {
    if data.len() < 8 || &data[4..8] != b"ftyp" {
        return None;
    }
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        if size < 8 { break; }
        if &data[pos + 4..pos + 8] == b"moov" {
            return Some(pos + size);
        }
        pos += size;
    }
    None
}

fn merge_fmp4_init(video_init: &[u8], audio_init: &[u8]) -> Option<Vec<u8>> {
    let (v_moov_pos, v_moov_size) = find_box(video_init, b"moov")?;
    let (a_moov_pos, a_moov_size) = find_box(audio_init, b"moov")?;
    let (v_ftyp_pos, v_ftyp_size) = find_box(video_init, b"ftyp")?;

    let v_moov_data = &video_init[v_moov_pos + 8..v_moov_pos + v_moov_size];
    let a_moov_data = &audio_init[a_moov_pos + 8..a_moov_pos + a_moov_size];

    let a_trak = find_box(a_moov_data, b"trak")?;
    let a_trak_bytes = &a_moov_data[a_trak.0..a_trak.0 + a_trak.1];

    let mut new_trak = a_trak_bytes.to_vec();

    if let Some((tkhd_rel, _)) = find_box(&new_trak, b"tkhd") {
        let version = new_trak[tkhd_rel + 8];
        let track_id_offset = if version == 1 {
            tkhd_rel + 8 + 4 + 8 + 8
        } else {
            tkhd_rel + 8 + 4 + 4 + 4
        };
        if track_id_offset + 4 <= new_trak.len() {
            new_trak[track_id_offset..track_id_offset + 4]
                .copy_from_slice(&2u32.to_be_bytes());
        }
    }

    let new_moov_children: Vec<u8> = {
        let mut children = Vec::new();
        let mut pos = 0;
        while pos + 8 <= v_moov_data.len() {
            let sz = u32::from_be_bytes(v_moov_data[pos..pos + 4].try_into().ok()?)
                .max(8) as usize;
            if pos + sz > v_moov_data.len() { break; }
            children.extend_from_slice(&v_moov_data[pos..pos + sz]);
            pos += sz;
        }
        children.extend_from_slice(&new_trak);
        children
    };

    let new_moov_size = 8 + new_moov_children.len();
    let ftyp_bytes = &video_init[v_ftyp_pos..v_ftyp_pos + v_ftyp_size];

    let mut out = Vec::with_capacity(ftyp_bytes.len() + new_moov_size);
    out.extend_from_slice(ftyp_bytes);
    out.extend_from_slice(&(new_moov_size as u32).to_be_bytes());
    out.extend_from_slice(b"moov");
    out.extend_from_slice(&new_moov_children);

    Some(out)
}

fn scan_media_moofs(data: &[u8]) -> Vec<(u64, u32)> {
    let mut results = Vec::new();
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let Ok(arr) = data[pos..pos + 4].try_into() else { break };
        let size = u32::from_be_bytes(arr).max(8) as usize;
        if pos + size > data.len() { break; }
        if &data[pos + 4..pos + 8] == b"moof" {
            results.push((pos as u64, size as u32));
        }
        pos += size;
    }
    results
}

fn scan_media_dat(data: &[u8]) -> Vec<(u64, u32)> {
    let mut results = Vec::new();
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let Ok(arr) = data[pos..pos + 4].try_into() else { break };
        let size = u32::from_be_bytes(arr).max(8) as usize;
        if pos + size > data.len() { break; }
        if &data[pos + 4..pos + 8] == b"mdat" {
            results.push((pos as u64, size as u32));
        }
        pos += size;
    }
    results
}

fn fix_audio_track_id(data: &mut [u8]) {
    if let Some((tfhd_pos, _)) = find_box(data, b"tfhd") {
        if tfhd_pos + 12 <= data.len() {
            data[tfhd_pos + 8..tfhd_pos + 12]
                .copy_from_slice(&2u32.to_be_bytes());
        }
    }
}

async fn fetch_to_file(
    client: &reqwest::Client,
    url: &str,
    visitor_data: &str,
    cookie: &str,
) -> Result<(tempfile::NamedTempFile, u64), BackendError> {
    use futures_util::StreamExt;
    use std::io::Write;

    let mut req = client.get(url);
    for (k, v) in build_cdn_headers(visitor_data, cookie) {
        req = req.header(k, v);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| BackendError::DownloadFailed(format!("fetch: {}", e)))?;

    let status = resp.status();
    if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        let body = resp.text().await.unwrap_or_default();
        return Err(BackendError::DownloadFailed(format!(
            "HTTP {} - {}",
            status,
            truncate_str(&body, 200)
        )));
    }

    let mut tmp = tempfile::NamedTempFile::new()
        .map_err(|e| BackendError::Io(e))?;
    let mut stream = resp.bytes_stream();
    let mut total: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| BackendError::DownloadFailed(format!("stream: {}", e)))?;
        total += chunk.len() as u64;
        tmp.write_all(&chunk)
            .map_err(|e| BackendError::Io(e))?;
    }
    tmp.flush().map_err(|e| BackendError::Io(e))?;
    Ok((tmp, total))
}

async fn merge_streams(
    client: &reqwest::Client,
    video_url: String,
    audio_url: String,
    title: &str,
    video_mime: &str,
    visitor_data: &str,
    cookie: &str,
) -> Result<DownloadOutput, BackendError> {
    use std::io::{Read, Seek, Write};

    tracing::info!("[purerust] merge: streaming video to temp file...");
    let (mut v_tmp, v_len) = fetch_to_file(client, &video_url, visitor_data, cookie).await?;
    tracing::info!("[purerust] merge: streaming audio to temp file...");
    let (mut a_tmp, a_len) = fetch_to_file(client, &audio_url, visitor_data, cookie).await?;

    tracing::info!("[purerust] merge: video={} bytes, audio={} bytes", v_len, a_len);

    let ext = ext_for_mime(video_mime);
    let out_path = std::env::temp_dir().join(format!("yt_merged_{}.{}", std::process::id(), ext));
    let mut out = std::fs::File::create(&out_path)
        .map_err(|e| BackendError::Io(e))?;

    let merged_total: u64;

    // Read file headers to detect fMP4
    let header_size = 8192.min(v_len as usize).min(a_len as usize);
    let mut v_header = vec![0u8; header_size];
    let mut a_header = vec![0u8; header_size];
    v_tmp.seek(std::io::SeekFrom::Start(0)).map_err(|e| BackendError::Io(e))?;
    a_tmp.seek(std::io::SeekFrom::Start(0)).map_err(|e| BackendError::Io(e))?;
    v_tmp.read_exact(&mut v_header).map_err(|e| BackendError::Io(e))?;
    a_tmp.read_exact(&mut a_header).map_err(|e| BackendError::Io(e))?;

    let v_init_size = find_init_size(&v_header);
    let a_init_size = find_init_size(&a_header);

    if v_init_size.is_none() || a_init_size.is_none() {
        tracing::info!("[purerust] merge: non-fMP4, raw concatenating files");
        v_tmp.seek(std::io::SeekFrom::Start(0)).map_err(|e| BackendError::Io(e))?;
        a_tmp.seek(std::io::SeekFrom::Start(0)).map_err(|e| BackendError::Io(e))?;
        std::io::copy(&mut v_tmp, &mut out).map_err(|e| BackendError::Io(e))?;
        std::io::copy(&mut a_tmp, &mut out).map_err(|e| BackendError::Io(e))?;
        merged_total = v_len + a_len;
    } else {
        let v_init_size = v_init_size.unwrap();
        let a_init_size = a_init_size.unwrap();

        // Read full init segments into memory (small)
        let mut v_init = vec![0u8; v_init_size];
        let mut a_init = vec![0u8; a_init_size];
        v_tmp.seek(std::io::SeekFrom::Start(0)).map_err(|e| BackendError::Io(e))?;
        a_tmp.seek(std::io::SeekFrom::Start(0)).map_err(|e| BackendError::Io(e))?;
        v_tmp.read_exact(&mut v_init).map_err(|e| BackendError::Io(e))?;
        a_tmp.read_exact(&mut a_init).map_err(|e| BackendError::Io(e))?;

        // Read remaining media bytes to scan boxes
        let v_media_size = (v_len as usize).saturating_sub(v_init_size);
        let a_media_size = (a_len as usize).saturating_sub(a_init_size);
        let mut v_media = vec![0u8; v_media_size];
        let mut a_media = vec![0u8; a_media_size];
        v_tmp.read_exact(&mut v_media).map_err(|e| BackendError::Io(e))?;
        a_tmp.read_exact(&mut a_media).map_err(|e| BackendError::Io(e))?;

        // Let the temp files go (will be deleted on drop)
        drop(v_tmp);
        drop(a_tmp);

        // Merge init
        let merged_init = merge_fmp4_init(&v_init, &a_init)
            .unwrap_or_else(|| {
                tracing::warn!("[purerust] merge: moov merge failed, using raw concat init");
                let mut fb = Vec::with_capacity(v_init.len() + a_init.len());
                fb.extend_from_slice(&v_init);
                fb.extend_from_slice(&a_init);
                fb
            });
        out.write_all(&merged_init).map_err(|e| BackendError::Io(e))?;

        // Scan media segments
        let v_moofs = scan_media_moofs(&v_media);
        let a_moofs = scan_media_moofs(&a_media);
        let v_mdats = scan_media_dat(&v_media);
        let a_mdats = scan_media_dat(&a_media);

        let total = v_moofs.len().max(a_moofs.len());
        tracing::info!("[purerust] merge: {} video moofs, {} audio moofs", v_moofs.len(), a_moofs.len());

        for i in 0..total {
            if let Some(&(pos, size)) = v_moofs.get(i) {
                let pos = pos as usize;
                let size = size as usize;
                out.write_all(&v_media[pos..pos + size]).map_err(|e| BackendError::Io(e))?;
                if let Some(&(mpos, msize)) = v_mdats.get(i) {
                    let mpos = mpos as usize;
                    let msize = msize as usize;
                    out.write_all(&v_media[mpos..mpos + msize]).map_err(|e| BackendError::Io(e))?;
                }
            }
            if let Some(&(pos, size)) = a_moofs.get(i) {
                let pos = pos as usize;
                let size = size as usize;
                let mut moof = a_media[pos..pos + size].to_vec();
                fix_audio_track_id(&mut moof);
                out.write_all(&moof).map_err(|e| BackendError::Io(e))?;
                if let Some(&(mpos, msize)) = a_mdats.get(i) {
                    let mpos = mpos as usize;
                    let msize = msize as usize;
                    out.write_all(&a_media[mpos..mpos + msize]).map_err(|e| BackendError::Io(e))?;
                }
            }
        }

        merged_total = out.stream_position().map_err(|e| BackendError::Io(e))?;
    }

    tracing::info!("[purerust] merge: final output {} bytes", merged_total);

    let file = tokio::fs::File::open(&out_path)
        .await
        .map_err(|e| BackendError::Io(e))?;

    Ok(DownloadOutput {
        filename: format!("{}.{}", title.trim(), ext),
        content_type: format!("video/{}", ext),
        reader: Box::new(file),
        size_hint: Some(merged_total),
    })
}
