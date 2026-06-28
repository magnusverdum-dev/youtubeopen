use std::sync::Arc;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;

mod api;
mod backends;
mod core;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let state = Arc::new(api::routes::AppState {
        ytdlp: backends::ytdlp::YtDlpBackend,
        purerust: backends::purerust::PureRustBackend::new(),
    });

    let app = api::routes::create_router(state)
        .fallback_service(ServeDir::new("frontend-dist").append_index_html_on_directories(true))
        .layer(CompressionLayer::new().br(true).gzip(true));

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap();
    tracing::info!("listening on http://{}", addr);

    axum::serve(listener, app).await.unwrap();
}
