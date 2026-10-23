//! Reports of crawls to Telegram: what a crawl found, and what changed
//! since the site's previous crawl, sent by a bot whose token is in
//! SEO_SPIDER_TELEGRAM_TOKEN (an environment variable, not a flag: flags
//! show in the process list).

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use seo_store::Store;

/// Telegram's Bot API; SEO_SPIDER_TELEGRAM_API points elsewhere in tests.
const API: &str = "https://api.telegram.org";

#[derive(Clone)]
pub struct Telegram {
    token: String,
    api: String,
    client: reqwest::Client,
}

impl Telegram {
    /// The bot of SEO_SPIDER_TELEGRAM_TOKEN, when it is set.
    pub fn from_env() -> Option<Telegram> {
        let token = std::env::var("SEO_SPIDER_TELEGRAM_TOKEN").ok()?.trim().to_string();
        if token.is_empty() {
            return None;
        }
        let api = std::env::var("SEO_SPIDER_TELEGRAM_API").unwrap_or_else(|_| API.into());
        Some(Telegram { token, api: api.trim_end_matches('/').to_string(), client: reqwest::Client::new() })
    }

    /// Sends `html` (Telegram's HTML subset) to `chat`: a chat id, or
    /// @channel.
    pub async fn send(&self, chat: &str, html: &str) -> Result<()> {
        let payload = serde_json::json!({
            "chat_id": chat.trim(),
            "text": html,
            "parse_mode": "HTML",
            "link_preview_options": {"is_disabled": true},
        });
        // The token is in the address: errors leave the address out.
        let response = self
            .client
            .post(format!("{}/bot{}/sendMessage", self.api, self.token))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&payload)?)
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .map_err(|error| anyhow!("Telegram недоступен: {}", error.without_url()))?;
        let bytes = response.bytes().await.map_err(|error| anyhow!("ответ Telegram: {}", error.without_url()))?;
        let reply: serde_json::Value = serde_json::from_slice(&bytes).context("ответ Telegram")?;
        if reply["ok"].as_bool() != Some(true) {
            bail!("Telegram: {}", reply["description"].as_str().unwrap_or("отказ без объяснения"));
        }
        Ok(())
    }
}

/// Text for Telegram's HTML: `<`, `>` and `&` escaped.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// 12 345.
fn count(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut grouped = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            grouped.push(' ');
        }
        grouped.push(digit);
    }
    if value < 0 {
        format!("−{grouped}")
    } else {
        grouped
    }
}

/// The site as people name it: its host, and port when it has one.
fn host(start_url: &str) -> String {
    let Ok(url) = url::Url::parse(start_url) else { return start_url.to_string() };
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        _ => start_url.to_string(),
    }
}

/// The report of crawl `id`: what it found and, against the site's
/// previous crawl, what changed; `link` opens the crawl in the UI.
pub fn report(store: &Store, id: i64, link: Option<&str>) -> Result<String> {
    let info = store.crawls()?.into_iter().find(|crawl| crawl.id == id).ok_or_else(|| anyhow!("нет обхода {id}"))?;
    let overview = store.overview(id)?;
    let status = |key: &str| overview.statuses.iter().find(|(status, _)| status == key).map_or(0, |(_, count)| *count);
    let errors = status("4xx") + status("5xx") + status("no_response");
    let serious = store
        .issue_counts(id)?
        .into_iter()
        .filter(|(code, urls)| *urls > 0 && seo_checks::find(code).is_some_and(|check| check.severity.as_str() == "high"))
        .count();
    let state = if info.stopped { "остановлен" } else { "готов" };
    let mut lines = vec![
        format!("<b>{}</b>: обход #{id} {state}", escape(&host(&info.start_url))),
        format!(
            "{} URL, {} страниц, ошибок ответа {}, серьёзных проблем {}",
            count(overview.urls),
            count(overview.pages),
            count(errors),
            serious
        ),
    ];
    if let Some(previous) = store.previous_crawl(id)? {
        let (diff, _) = crate::compare::diff(store, previous, id)?;
        let changed: std::collections::HashSet<i64> = diff.changed.iter().map(|change| change.id).collect();
        lines.push(String::new());
        lines.push(format!(
            "С обхода #{previous}: новых URL {}, исчезли {}, изменились {}",
            count(diff.added.len() as i64),
            count(diff.removed.len() as i64),
            count(changed.len() as i64)
        ));
        lines.push(format!(
            "Проблем новых {}, исправлено {}",
            count(diff.new_issues.len() as i64),
            count(diff.fixed_issues.len() as i64)
        ));
        for shift in diff.shifts.iter().filter(|shift| shift.before != shift.after).take(3) {
            let title = seo_checks::find(&shift.code).map_or(shift.code.as_str(), |check| check.title);
            lines.push(format!("{}: {} → {}", escape(title), count(shift.before), count(shift.after)));
        }
    }
    if let Some(link) = link {
        lines.push(String::new());
        lines.push(format!("<a href=\"{}\">Открыть обход</a>", escape(link).replace('"', "&quot;")));
    }
    Ok(lines.join("\n"))
}

/// A crawl that could not start or failed, for the same chat.
pub fn failure(start_url: &str, error: &str) -> String {
    format!("<b>{}</b>: обход не удался\n{}", escape(&host(start_url)), escape(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_text_read_well() {
        assert_eq!([0, 999, 1000, 1_234_567, -45_000].map(count), ["0", "999", "1 000", "1 234 567", "−45 000"]);
        assert_eq!(escape("<b>A & B</b>"), "&lt;b&gt;A &amp; B&lt;/b&gt;");
        assert_eq!(failure("https://e.com/a", "x < y"), "<b>e.com</b>: обход не удался\nx &lt; y");
        assert_eq!(host("http://127.0.0.1:8099/"), "127.0.0.1:8099");
    }

    /// A bot API that answers like Telegram's and keeps what it was sent.
    async fn fake_api(ok: bool) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut buffer = [0u8; 8192];
                loop {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    let Some(end) = text.find("\r\n\r\n") else { continue };
                    let length: usize = text[..end]
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse().unwrap_or(0)))
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
                kept.lock().unwrap().push(String::from_utf8_lossy(&request).to_string());
                let body = if ok { r#"{"ok":true,"result":{}}"# } else { r#"{"ok":false,"description":"Bad Request: chat not found"}"# };
                let reply = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}", body.len());
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        (address, seen)
    }

    #[tokio::test]
    async fn sends_to_the_bot_api() {
        let (api, seen) = fake_api(true).await;
        let bot = Telegram { token: "123:secret".into(), api, client: reqwest::Client::new() };
        bot.send(" -1001 ", "<b>hi</b>").await.unwrap();
        let request = seen.lock().unwrap()[0].clone();
        assert!(request.starts_with("POST /bot123:secret/sendMessage "), "{request}");
        assert!(request.contains(r#""chat_id":"-1001""#) && request.contains(r#""parse_mode":"HTML""#), "{request}");

        let (api, _) = fake_api(false).await;
        let refused = Telegram { token: "123:secret".into(), api, client: reqwest::Client::new() };
        let error = refused.send("x", "hi").await.unwrap_err().to_string();
        assert!(error.contains("chat not found") && !error.contains("secret"), "{error}");
        let nowhere = Telegram { token: "123:secret".into(), api: "http://127.0.0.1:9".into(), client: reqwest::Client::new() };
        let error = nowhere.send("x", "hi").await.unwrap_err().to_string();
        assert!(!error.contains("secret"), "{error}");
    }
}
