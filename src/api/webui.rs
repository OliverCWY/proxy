use axum::{
    body::Body,
    http::{StatusCode, Uri, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "webui/dist/"]
pub struct WebAssets;

pub async fn root_redirect() -> impl IntoResponse {
    Redirect::temporary("/ui/")
}

pub async fn ui_root_redirect() -> impl IntoResponse {
    Redirect::temporary("/ui/")
}

pub async fn static_handler(uri: Uri) -> Response {
    let mut path = uri.path().trim_start_matches('/').to_string();

    // Strip "ui/" prefix if present
    if let Some(stripped) = path.strip_prefix("ui/") {
        path = stripped.to_string();
    } else if path == "ui" {
        path = "".to_string();
    }

    if path.is_empty() {
        path = "index.html".to_string();
    }

    // 1. Try to serve exact embedded file
    if let Some(file) = WebAssets::get(&path) {
        let mime = mime_guess::from_path(&path).first_or_octet_stream();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            Body::from(file.data),
        )
            .into_response();
    }

    // 2. If path has file extension (e.g. missing asset .js, .css) -> 404
    if std::path::Path::new(&path)
        .extension()
        .and_then(|s| s.to_str())
        .is_some()
    {
        return (StatusCode::NOT_FOUND, "Asset not found").into_response();
    }

    // 3. SPA History Fallback: fallback to index.html for client-side routing
    if let Some(index_file) = WebAssets::get("index.html") {
        return Html(index_file.data).into_response();
    }

    (StatusCode::NOT_FOUND, "Web UI not embedded").into_response()
}
