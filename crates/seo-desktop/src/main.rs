//! seo-spider as a desktop app: the server runs inside it on a free local
//! port, its API behind a token made at start, and a window shows the UI
//! built into it. Crawls go into a SQLite file in the app's data folder,
//! files the UI exports into Downloads.

// No console window next to the app on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};

use tauri::webview::{DownloadEvent, WebviewWindowBuilder};
use tauri::{Manager, WebviewUrl};

// As the CLI: mimalloc, freed pages back after 100 ms (see seo-cli).
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

extern "C" {
    fn mi_option_set(option: std::ffi::c_int, value: std::ffi::c_long);
}

const MI_OPTION_PURGE_DELAY: std::ffi::c_int = 15;

/// 128 bits from the OS's randomness (std seeds its hashers from it).
fn token() -> String {
    (0..2u64)
        .map(|part| {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u64(part);
            format!("{:016x}", hasher.finish())
        })
        .collect()
}

/// Where an export goes: the folder, under its own name (or the address's
/// last part), never over a file already there.
fn place(folder: &Path, url: &tauri::Url, suggested: &Path) -> PathBuf {
    let named = |name: &str| name.contains('.').then(|| name.to_string());
    let name = suggested
        .file_name()
        .and_then(|name| named(&name.to_string_lossy()))
        .or_else(|| url.path_segments().and_then(|mut parts| parts.next_back()).and_then(named))
        .unwrap_or_else(|| "seo-spider-export".into());
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) => (stem.to_string(), format!(".{extension}")),
        None => (name.clone(), String::new()),
    };
    let mut path = folder.join(&name);
    let mut copy = 1;
    while path.exists() {
        path = folder.join(format!("{stem} ({copy}){extension}"));
        copy += 1;
    }
    path
}

fn main() {
    if std::env::var_os("MIMALLOC_PURGE_DELAY").is_none() {
        // SAFETY: sets an option of the allocator linked in above.
        unsafe { mi_option_set(MI_OPTION_PURGE_DELAY, 100) };
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("seo_crawl=info,obscura::console=off,obscura_js=error,obscura_browser=error,usvg=error,warn")
            }),
        )
        .with_writer(std::io::stderr)
        .init();
    tauri::Builder::default()
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let downloads = app.path().download_dir().unwrap_or_else(|_| data.clone());
            // Bound here, so the window can open before the server accepts.
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let address = listener.local_addr()?;
            let token = token();
            let options = seo_server::ServerOptions {
                db: data.join("crawl.sqlite"),
                bind: address,
                ui_dir: None,
                token: Some(token.clone()),
                public_url: None,
            };
            std::thread::Builder::new().name("server".into()).spawn(move || {
                let served = tokio::runtime::Runtime::new().map_err(anyhow::Error::from).and_then(|runtime| {
                    runtime.block_on(async move {
                        listener.set_nonblocking(true)?;
                        seo_server::serve_on(tokio::net::TcpListener::from_std(listener)?, options).await
                    })
                });
                if let Err(error) = served {
                    eprintln!("seo-spider server: {error:#}");
                }
            })?;
            let url = tauri::Url::parse(&format!("http://{address}/?token={token}"))?;
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("seo-spider")
                .inner_size(1440.0, 900.0)
                .min_inner_size(960.0, 600.0)
                .on_download(move |_, event| {
                    if let DownloadEvent::Requested { url, destination } = event {
                        *destination = place(&downloads, &url, destination);
                    }
                    true
                })
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("seo-spider");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_land_beside_not_over() {
        let folder = std::env::temp_dir().join(format!("seo-desktop-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let url = tauri::Url::parse("http://127.0.0.1:1/api/crawls/3/export.xlsx?token=x").unwrap();
        assert_eq!(place(&folder, &url, Path::new("")), folder.join("export.xlsx"));
        assert_eq!(place(&folder, &url, Path::new("/tmp/broken-links-3.csv")), folder.join("broken-links-3.csv"));
        std::fs::write(folder.join("export.xlsx"), b"").unwrap();
        assert_eq!(place(&folder, &url, Path::new("")), folder.join("export (1).xlsx"));
        let _ = std::fs::remove_dir_all(&folder);
        assert_eq!(token().len(), 32);
        assert_ne!(token(), token());
    }
}
