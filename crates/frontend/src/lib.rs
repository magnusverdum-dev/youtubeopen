use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn run() {
    dioxus::launch(App);
}

use dioxus::prelude::*;
use gloo_net::http::Request;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct FormatEntry {
    format_id: String,
    ext: String,
    resolution: String,
    filesize: Option<u64>,
    fps: Option<f64>,
    vcodec: String,
    acodec: String,
}

#[derive(Debug, Clone, Deserialize)]
struct VideoInfo {
    title: String,
    duration: Option<f64>,
    thumbnail: Option<String>,
    formats: Vec<FormatEntry>,
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HealthResponse {
    backend: Option<String>,
    name: Option<String>,
    available: Option<bool>,
    version: Option<String>,
}

fn do_health_check(
    mut health_text: Signal<String>,
    mut health_class: Signal<String>,
    mut fetch_disabled: Signal<bool>,
    mut status: Signal<String>,
    mut status_class: Signal<String>,
) {
    spawn(async move {
        match Request::get(&format!("/api/health?backend={BACKEND}")).send().await {
            Ok(resp) => {
                if let Ok(h) = resp.json::<HealthResponse>().await {
                    if h.available.unwrap_or(false) {
                        health_text.set(h.version.unwrap_or("healthy".to_string()));
                        health_class.set("health healthy".to_string());
                        fetch_disabled.set(false);
                    } else {
                        health_text.set("unavailable".to_string());
                        health_class.set("health unhealthy".to_string());
                        fetch_disabled.set(true);
                        status.set(format!("{} NOT AVAILABLE",
                            h.name.unwrap_or("Backend".to_string())));
                        status_class.set("error".to_string());
                    }
                }
            }
            Err(_) => {
                health_text.set("offline".to_string());
                health_class.set("health unhealthy".to_string());
                fetch_disabled.set(true);
                status.set("SERVER OFFLINE".to_string());
                status_class.set("error".to_string());
            }
        }
    });
}

fn do_fetch_info(
    url: Signal<String>,
    mut status: Signal<String>,
    mut status_class: Signal<String>,
    mut error_msg: Signal<String>,
    mut video_title: Signal<String>,
    mut video_duration: Signal<String>,
    mut formats: Signal<Vec<FormatEntry>>,
    mut info_visible: Signal<bool>,
    mut loading: Signal<bool>,
    mut fetch_disabled: Signal<bool>,
) {
    spawn(async move {
        let u = url().trim().to_string();
        if u.is_empty() {
            status.set("ENTER A VALID URL".to_string());
            status_class.set("error".to_string());
            return;
        }
        loading.set(true);
        fetch_disabled.set(true);
        error_msg.set(String::new());
        info_visible.set(false);
        status.set("FETCHING VIDEO INFO...".to_string());
        status_class.set("info".to_string());

        let encoded_url = js_sys::encode_uri_component(&u)
            .as_string().unwrap_or_else(|| u.clone());

        match Request::get(&format!(
            "/api/info?url={}&backend={BACKEND}", encoded_url
        )).send().await {
            Ok(resp) => {
                if let Ok(data) = resp.json::<VideoInfo>().await {
                    if let Some(err) = &data.error {
                        error_msg.set(err.clone());
                        status.set("FETCH FAILED".to_string());
                        status_class.set("error".to_string());
                    } else {
                        video_title.set(data.title);
                        let dur = data.duration
                            .map(format_duration)
                            .unwrap_or_default();
                        video_duration.set(dur);
                        let filtered: Vec<FormatEntry> = data.formats.into_iter()
                            .filter(|f| f.vcodec != "none")
                            .collect();
                        formats.set(filtered);
                        info_visible.set(true);
                        status.set("VIDEO FOUND".to_string());
                        status_class.set(String::new());
                    }
                }
            }
            Err(e) => {
                error_msg.set(format!("NETWORK ERROR: {}", e));
                status.set("CONNECTION FAILED".to_string());
                status_class.set("error".to_string());
            }
        }
        loading.set(false);
        fetch_disabled.set(false);
    });
}

const BACKEND: &str = "ytdlp";

fn App() -> Element {
    let mut url = use_signal(|| String::new());
    let mut status = use_signal(|| "READY".to_string());
    let status_class = use_signal(|| "".to_string());
    let error_msg = use_signal(|| String::new());
    let video_title = use_signal(|| String::new());
    let video_duration = use_signal(|| String::new());
    let formats = use_signal(Vec::<FormatEntry>::new);
    let info_visible = use_signal(|| false);
    let loading = use_signal(|| false);
    let health_text = use_signal(|| "checking...".to_string());
    let health_class = use_signal(|| "".to_string());
    let fetch_disabled = use_signal(|| true);

    use_effect(move || {
        do_health_check(
            health_text.clone(), health_class.clone(),
            fetch_disabled.clone(), status.clone(), status_class.clone(),
        );
    });

    let on_fetch = move || {
        do_fetch_info(
            url.clone(), status.clone(), status_class.clone(),
            error_msg.clone(), video_title.clone(), video_duration.clone(),
            formats.clone(), info_visible.clone(), loading.clone(), fetch_disabled.clone(),
        );
    };

    let mut on_download = move |fmt: FormatEntry| {
        let u = url();
        let download_url = format!(
            "/api/download?url={}&format_id={}&merge_audio=true&backend={BACKEND}",
            js_sys::encode_uri_component(&u)
                .as_string().unwrap_or_else(|| u.clone()),
            js_sys::encode_uri_component(&fmt.format_id)
                .as_string().unwrap_or_else(|| fmt.format_id.clone()),
        );
        if let Some(win) = web_sys::window() {
            let _ = win.open_with_url_and_target(&download_url, "_blank");
        }
        status.set("DOWNLOAD STARTED - CHECK BROWSER DOWNLOADS".to_string());
    };

    rsx! {
        div { class: "container",
            h1 { "YouTubeOpen" }
            div { class: "subtitle", "100% Rust - Axum + Dioxus" }

            div { class: "panel",
                div { class: "panel-header",
                    span { class: "panel-title", "DOWNLOAD" }
                    span { class: "{health_class()}", "{health_text()}" }
                }

                div { class: "url-row",
                    input {
                        class: "url-input",
                        value: "{url}",
                        oninput: move |e| url.set(e.value()),
                        placeholder: "https://youtube.com/watch?v=...",
                        onkeydown: move |e| if e.key() == Key::Enter { on_fetch() },
                    }
                    button {
                        class: "btn-fetch",
                        disabled: fetch_disabled(),
                        onclick: move |_| on_fetch(),
                        "[ FETCH ]"
                    }
                }

                div { class: "status {status_class()}", "{status()}" }

                div {
                    display: if loading() { "block" } else { "none" },
                    class: "progress",
                    div { class: "progress-bar",
                        div { class: "progress-fill" }
                    }
                    div { class: "progress-text", "FETCHING..." }
                }

                div {
                    display: if !error_msg().is_empty() { "block" } else { "none" },
                    class: "error-msg",
                    "{error_msg()}"
                }

                div {
                    display: if info_visible() { "block" } else { "none" },
                    class: "info-panel",
                    div { class: "video-title", "{video_title()}" }
                    div { class: "video-meta", "Duration: {video_duration()}" }
                    div { class: "format-list",
                        {
                            let fmts = formats();
                            fmts.into_iter().map(|fmt| {
                                let onclick = {
                                    let f = fmt.clone();
                                    move |_| on_download(f.clone())
                                };
                                let key = fmt.format_id.clone();
                                rsx! {
                                    FormatButton {
                                        key: "{key}",
                                        fmt: fmt,
                                        on_download: onclick
                                    }
                                }
                            })
                        }
                    }
                }
            }

            div { class: "local-note",
                "Live at ",
                a { href: "https://youtubeopen.fly.dev", "youtubeopen.fly.dev" }
                " | 100% Rust | Axum + Dioxus"
            }
        }
    }
}

#[component]
fn FormatButton(fmt: FormatEntry, on_download: EventHandler<MouseEvent>) -> Element {
    let size = match fmt.filesize {
        Some(s) => format_size(s),
        None => "--".to_string(),
    };
    let fps = match fmt.fps {
        Some(f) => format!(" {}fps", f as u32),
        None => String::new(),
    };
    rsx! {
        button {
            class: "format-btn",
            onclick: move |e| on_download.call(e),
            span { class: "format-main",
                "{fmt.resolution}"
                span { class: "format-meta", " [{fmt.ext}]" }
            }
            span { class: "format-size", "{size}{fps}" }
        }
    }
}

fn format_duration(seconds: f64) -> String {
    let secs = seconds as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut i = 0;
    while size >= 1024.0 && i < UNITS.len() - 1 {
        size /= 1024.0;
        i += 1;
    }
    if i > 0 {
        format!("{:.1} {}", size, UNITS[i])
    } else {
        format!("{} {}", size as u64, UNITS[i])
    }
}