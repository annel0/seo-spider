//! One HTTP request without following redirects: a redirect is a response of
//! its own and the crawler queues its target, so chains come out hop by hop.

use std::time::{Duration, Instant};

use reqwest::Client;
use seo_page::HttpInfo;
use url::Url;

pub struct ClientOptions<'a> {
    pub user_agent: &'a str,
    pub timeout: Duration,
    pub concurrency: usize,
}

pub fn client(options: &ClientOptions<'_>) -> anyhow::Result<Client> {
    Ok(Client::builder()
        // Over HTTP/2 a status check drops its response unread, and further
        // streams on the same connection then stall until the timeout (seen
        // on Cloudflare hosts; obscura-net hit the same and defaults to
        // HTTP/1.1 for it). One request per connection keeps them apart.
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(options.user_agent)
        .timeout(options.timeout)
        .connect_timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(options.concurrency)
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .build()?)
}

/// Requests `url`; reads the body only when `want_body` and the response is
/// a successful HTML document, up to `max_body` bytes. `timeout` overrides
/// the client's for this request.
pub async fn fetch(
    client: &Client,
    url: &Url,
    want_body: bool,
    max_body: usize,
    timeout: Option<Duration>,
) -> (HttpInfo, Option<String>) {
    let started = Instant::now();
    let mut http = HttpInfo { url: url.to_string(), ..Default::default() };
    let mut request = client.get(url.clone());
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            http.ttfb_ms = started.elapsed().as_millis() as u32;
            http.error = Some(describe(&error));
            return (http, None);
        }
    };
    http.ttfb_ms = started.elapsed().as_millis() as u32;
    http.status = response.status().as_u16();
    http.headers = response
        .headers()
        .iter()
        .map(|(name, value)| (name.as_str().to_string(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
        .collect();
    http.content_type = http.header("content-type").map(str::to_string);
    http.location = http
        .header("location")
        .and_then(|location| url.join(location.trim()).ok())
        .map(|mut target| {
            target.set_fragment(None);
            target.to_string()
        });
    if !(want_body && response.status().is_success() && http.is_html()) {
        http.size = response.content_length().unwrap_or(0);
        return (http, None);
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                body.extend_from_slice(&chunk);
                if body.len() > max_body {
                    body.truncate(max_body);
                    http.error = Some(format!("body cut at {max_body} bytes"));
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => {
                http.error = Some(describe(&error));
                break;
            }
        }
    }
    http.size = body.len() as u64;
    let html = decode(&body, http.content_type.as_deref());
    (http, Some(html))
}

/// Decodes a document by the charset of its `Content-Type`, then of a
/// `<meta charset>` near the top, falling back to UTF-8.
fn decode(body: &[u8], content_type: Option<&str>) -> String {
    let label = content_type
        .and_then(charset_param)
        .or_else(|| sniff_meta_charset(&body[..body.len().min(2048)]));
    let encoding = label
        .and_then(|label| encoding_rs::Encoding::for_label(label.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(body);
    text.into_owned()
}

fn charset_param(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|param| {
        let (key, value) = param.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches('"').to_string())
    })
}

fn sniff_meta_charset(head: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(head).to_ascii_lowercase();
    let at = head.find("charset=")? + "charset=".len();
    let value: String = head[at..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    (!value.is_empty()).then_some(value)
}

/// The error with its causes, the way a report shows it ("timeout", "dns
/// error: ...").
fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return "timeout".into();
    }
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_by_header_then_meta() {
        let cp1251 = [0xcf, 0xf0, 0xe8, 0xe2, 0xe5, 0xf2]; // "Привет"
        assert_eq!(decode(&cp1251, Some("text/html; charset=windows-1251")), "Привет");
        let mut doc = b"<meta charset=\"windows-1251\"><p>".to_vec();
        doc.extend_from_slice(&cp1251);
        assert!(decode(&doc, Some("text/html")).ends_with("Привет"));
        assert_eq!(decode("ok".as_bytes(), None), "ok");
    }
}
