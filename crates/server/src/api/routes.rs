use axum::{
    body::Body,
    extract::Query,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::io::ReaderStream;

use crate::backends::{ytdlp::YtDlpBackend, purerust::PureRustBackend, BackendType};
use crate::core::VideoBackend;

pub struct AppState {
    pub ytdlp: YtDlpBackend,
    pub purerust: PureRustBackend,
}

fn resolve_backend<'a>(
    state: &'a AppState,
    params: &HashMap<String, String>,
) -> &'a dyn VideoBackend {
    let backend_str = params.get("backend").map(|s| s.as_str()).unwrap_or("ytdlp");
    BackendType::from_str(backend_str).backend(&state.ytdlp, &state.purerust)
}

pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/health", get(health_handler))
        .route("/api/info", get(info_handler))
        .route("/api/download", get(download_handler))
        .route("/api/debug-test", get(debug_handler))
        .with_state(state)
}

async fn health_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let ytdlp_health = state.ytdlp.health().await;
    let purerust_health = state.purerust.health().await;

    let backend_str = params.get("backend").map(|s| s.as_str()).unwrap_or("all");

    match backend_str {
        "ytdlp" => Json(serde_json::json!({
            "backend": "ytdlp",
            "name": ytdlp_health.backend_name,
            "available": ytdlp_health.available,
            "version": ytdlp_health.version,
        })),
        "purerust" => Json(serde_json::json!({
            "backend": "purerust",
            "name": purerust_health.backend_name,
            "available": purerust_health.available,
            "version": purerust_health.version,
        })),
        _ => Json(serde_json::json!({
            "ytdlp": {
                "name": ytdlp_health.backend_name,
                "available": ytdlp_health.available,
                "version": ytdlp_health.version,
            },
            "purerust": {
                "name": purerust_health.backend_name,
                "available": purerust_health.available,
                "version": purerust_health.version,
            },
        })),
    }
}

async fn info_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<crate::core::VideoInfo> {
    let url = match params.get("url") {
        Some(u) => u,
        None => {
            return Json(crate::core::VideoInfo {
                title: String::new(),
                duration: None,
                thumbnail: None,
                formats: vec![],
                error: Some("missing url parameter".to_string()),
            })
        }
    };

    let backend = resolve_backend(&state, &params);
    match backend.get_video_info(url).await {
        Ok(info) => Json(info),
        Err(e) => Json(crate::core::VideoInfo {
            title: String::new(),
            duration: None,
            thumbnail: None,
            formats: vec![],
            error: Some(e.to_string()),
        }),
    }
}

async fn debug_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> axum::response::Response {
    let url = match params.get("url") {
        Some(u) => u,
        None => {
            let mut resp = Json(serde_json::json!({"error": "missing url"})).into_response();
            resp.headers_mut().insert("X-Debug-Handler", "HIT".parse().unwrap());
            return resp;
        }
    };
    let format_id = match params.get("format_id") {
        Some(f) => f,
        None => {
            let mut resp = Json(serde_json::json!({"error": "missing format_id"})).into_response();
            resp.headers_mut().insert("X-Debug-Handler", "HIT".parse().unwrap());
            return resp;
        }
    };
    let purerust = &state.purerust;
    if let Some(video_id) = crate::backends::purerust::extract_video_id(url) {
        match purerust.get_player_debug(&video_id).await {
            Ok(player) => {
                let itag: u32 = format_id.parse().unwrap_or(0);
                let vo_itags: Vec<u32> = player.video_only_streams.iter().map(|s| s.itag).collect();
                let audio_itags: Vec<u32> = player.audio_streams.iter().map(|s| s.itag).collect();
                if let Some(vs) = player.video_streams.iter().find(|s| s.itag == itag) {
                    let mut resp = Json(serde_json::json!({
                        "url": vs.url,
                        "has_pot": vs.url.contains("pot="),
                        "client_type": format!("{:?}", player.client_type),
                        "visitor_data": player.visitor_data,
                    })).into_response();
                    resp.headers_mut().insert("X-Debug-Handler", "HIT".parse().unwrap());
                    return resp;
                }
                if let Some(vo) = player.video_only_streams.iter().find(|s| s.itag == itag) {
                    let best_audio = player.audio_streams.iter().max_by_key(|a| a.bitrate);
                    let mut resp = Json(serde_json::json!({
                        "video_url": vo.url,
                        "video_has_pot": vo.url.contains("pot="),
                        "audio_url": best_audio.map(|a| a.url.clone()),
                        "audio_has_pot": best_audio.map(|a| a.url.contains("pot=")),
                        "client_type": format!("{:?}", player.client_type),
                        "visitor_data": player.visitor_data,
                    })).into_response();
                    resp.headers_mut().insert("X-Debug-Handler", "HIT".parse().unwrap());
                    return resp;
                }
                let mut resp = Json(serde_json::json!({
                    "error": format!("itag {} not found", itag),
                    "client_type": format!("{:?}", player.client_type),
                    "video_only_itags": vo_itags,
                    "audio_itags": audio_itags,
                })).into_response();
                resp.headers_mut().insert("X-Debug-Handler", "HIT".parse().unwrap());
                resp
            }
            Err(e) => {
                let mut resp = Json(serde_json::json!({"error": format!("player error: {}", e)})).into_response();
                resp.headers_mut().insert("X-Debug-Handler", "HIT".parse().unwrap());
                resp
            }
        }
    } else {
        let mut resp = Json(serde_json::json!({"error": "could not get url"})).into_response();
        resp.headers_mut().insert("X-Debug-Handler", "HIT".parse().unwrap());
        resp
    }
}

async fn download_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let url = match params.get("url") {
        Some(u) => u,
        None => return (StatusCode::BAD_REQUEST, "missing url parameter").into_response(),
    };

    let format_id = match params.get("format_id") {
        Some(f) => f,
        None => return (StatusCode::BAD_REQUEST, "missing format_id parameter").into_response(),
    };

    let title = params.get("title").map(|s| s.as_str()).unwrap_or("video");
    let merge_audio = params.get("merge_audio").map(|s| s == "true").unwrap_or(true);

    let backend = resolve_backend(&state, &params);
    match backend.download_video(url, format_id, title, merge_audio).await {
        Ok(output) => {
            let stream = ReaderStream::new(output.reader);
            let body = Body::from_stream(stream);

            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_str(&format!(
                    "attachment; filename=\"{}\"",
                    output.filename
                ))
                .unwrap(),
            );
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&output.content_type).unwrap(),
            );

            (headers, body).into_response()
        }
        Err(e) => {
            tracing::error!("download failed: backend={:?}, url={}, format={}, error={}", params.get("backend"), url, format_id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("download error: {}", e),
            )
                .into_response()
        }
    }
}
