//! The UI's files: from a directory (`--ui-dir`, the Vite build) or, with
//! the `embed-ui` feature, from the binary. A path that is no file gets
//! index.html, so the app's own routes load it.

use std::path::PathBuf;

use axum::Router;

pub fn router(dir: Option<PathBuf>) -> Router {
    match dir {
        Some(dir) => {
            let index = tower_http::services::ServeFile::new(dir.join("index.html"));
            Router::new().fallback_service(tower_http::services::ServeDir::new(dir).fallback(index))
        }
        None => embedded(),
    }
}

#[cfg(feature = "embed-ui")]
fn embedded() -> Router {
    use axum::http::{header, StatusCode, Uri};
    use axum::response::IntoResponse;

    #[derive(rust_embed::RustEmbed)]
    #[folder = "../../ui/dist"]
    struct Assets;

    Router::new().fallback(|uri: Uri| async move {
        let path = uri.path().trim_start_matches('/');
        let (path, file) = match Assets::get(path).filter(|_| !path.is_empty()) {
            Some(file) => (path, file),
            None => match Assets::get("index.html") {
                Some(file) => ("index.html", file),
                None => return StatusCode::NOT_FOUND.into_response(),
            },
        };
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        ([(header::CONTENT_TYPE, mime.as_ref().to_string())], file.data.into_owned()).into_response()
    })
}

#[cfg(not(feature = "embed-ui"))]
fn embedded() -> Router {
    Router::new().fallback(|| async {
        axum::response::Html(
            "<!doctype html><meta charset=utf-8><p>Интерфейс не встроен в эту сборку: соберите его \
             (<code>cd ui && npm ci && npm run build</code>) и запустите <code>seo-spider ui --ui-dir ui/dist</code> \
             или соберите бинарник с <code>--features embed-ui</code>.</p>",
        )
    })
}
