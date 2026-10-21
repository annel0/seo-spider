//! One HTTP request without following redirects: a redirect is a response of
//! its own and the crawler queues its target, so chains come out hop by hop.

use std::time::{Duration, Instant};

use reqwest::Client;
use seo_page::HttpInfo;
use url::Url;

pub struct ClientOptions<'a> {
    pub user_agent: &'a str,
    pub timeout: Duration,
    pub connect_timeout: Duration,
    pub concurrency: usize,
    pub headers: &'a std::collections::BTreeMap<String, String>,
    /// Keep the cookies responses set, for the crawl's length.
    pub cookies: bool,
}

pub fn client(options: &ClientOptions<'_>) -> anyhow::Result<Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in options.headers {
        headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())?,
            reqwest::header::HeaderValue::from_str(value)?,
        );
    }
    Ok(Client::builder()
        // Over HTTP/2 a status check drops its response unread, and further
        // streams on the same connection then stall until the timeout (seen
        // on Cloudflare hosts; obscura-net hit the same and defaults to
        // HTTP/1.1 for it). One request per connection keeps them apart.
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(options.user_agent)
        .default_headers(headers)
        .cookie_store(options.cookies)
        .timeout(options.timeout)
        .connect_timeout(options.connect_timeout)
        .pool_max_idle_per_host(options.concurrency)
        .tcp_nodelay(true)
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .build()?)
}

/// How much of a successful HTML response to read.
#[derive(Debug, Clone, Copy)]
pub enum BodyMode {
    /// Status and headers only.
    None,
    /// Up to the end of `<head>`, at most this many bytes.
    Head(usize),
    /// The whole body, at most this many bytes.
    Full(usize),
}

/// A response body as received and as text.
pub struct Body {
    pub bytes: Vec<u8>,
    pub html: String,
}

/// What a request came back with.
pub struct Fetched {
    pub http: HttpInfo,
    pub body: Option<Body>,
    /// No connection was made (DNS, TCP or TLS): the host failed, not the URL.
    pub unreachable: bool,
}

/// Requests `url` and reads as much of a successful HTML response as `mode`
/// asks for. `timeout` overrides the client's for this request. With `head`,
/// a status-only request is a HEAD (the connection stays reusable, where a
/// GET dropped after its headers costs a new one), falling back to GET for
/// a server that does not take HEAD.
pub async fn fetch(
    client: &Client,
    url: &Url,
    mode: BodyMode,
    timeout: Option<Duration>,
    head: bool,
) -> Fetched {
    let started = Instant::now();
    let mut http = HttpInfo { url: url.to_string(), ..Default::default() };
    let send = |method: reqwest::Method| {
        let mut request = client.request(method, url.clone());
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        request.send()
    };
    let head = head && matches!(mode, BodyMode::None);
    let mut sent = send(if head { reqwest::Method::HEAD } else { reqwest::Method::GET }).await;
    // Servers that refuse HEAD say 405 or 501, but many answer it with an
    // error a GET would not get: only a GET confirms a failure.
    if head && sent.as_ref().is_ok_and(|response| response.status().as_u16() >= 400) {
        sent = send(reqwest::Method::GET).await;
    }
    let mut response = match sent {
        Ok(response) => response,
        Err(error) => {
            http.ttfb_ms = started.elapsed().as_millis() as u32;
            http.error = Some(describe(&error));
            return Fetched { http, body: None, unreachable: host_failed(&error) };
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
    let (limit, head_only) = match mode {
        BodyMode::Full(limit) => (limit, false),
        BodyMode::Head(limit) => (limit, true),
        BodyMode::None => (0, false),
    };
    if limit == 0 || !response.status().is_success() || !http.is_html() {
        http.size = response.content_length().unwrap_or(0);
        return Fetched { http, body: None, unreachable: false };
    }
    let mut body = Vec::with_capacity(response.content_length().map_or(64 << 10, |length| length as usize).min(limit));
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let searched = body.len().saturating_sub(6);
                body.extend_from_slice(&chunk);
                if head_only {
                    if let Some(end) = head_end(&body, searched) {
                        body.truncate(end);
                        break;
                    }
                }
                if body.len() > limit {
                    body.truncate(limit);
                    if !head_only {
                        http.error = Some(format!("body cut at {limit} bytes"));
                    }
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
    // A head-only read stops early; the size is what was read.
    http.size = body.len() as u64;
    let html = decode(&body, http.content_type.as_deref());
    Fetched { http, body: Some(Body { bytes: body, html }), unreachable: false }
}

/// Where the `<head>` of a document ends: the position of `</head` or of
/// `<body`, ASCII case-insensitively, searching from `from`.
fn head_end(document: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(offset) = document[at..].iter().position(|&byte| byte == b'<') {
        let tag = at + offset;
        let rest = &document[tag + 1..];
        let starts = |name: &[u8]| rest.len() >= name.len() && rest[..name.len()].eq_ignore_ascii_case(name);
        if starts(b"/head") || starts(b"body") {
            return Some(tag);
        }
        at = tag + 1;
    }
    None
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

/// Whether no connection could be made for reasons of the host's own: no
/// DNS record, no answer, refused, a certificate that does not verify. A
/// connection reset is left out: busy hosts and middleboxes reset some
/// connections and take the next one.
fn host_failed(error: &reqwest::Error) -> bool {
    if !error.is_connect() {
        return false;
    }
    // The I/O error sits deep in hyper's chain, wrapped in boxes that do not
    // all downcast; its text is what is reliably there.
    let text = describe(error).to_ascii_lowercase();
    !["reset", "aborted", "broken pipe", "unexpected eof"].iter().any(|word| text.contains(word))
}

/// The error with its causes, the way a report shows it ("timeout", "dns
/// error: ...").
fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        return if error.is_connect() { "connect timeout" } else { "timeout" }.into();
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
    fn finds_the_end_of_head() {
        let doc = b"<html><HEAD><title>t</title></Head><body>x";
        assert_eq!(head_end(doc, 0), Some(28));
        assert_eq!(head_end(b"<head><title>t</title><BODY>", 0), Some(22));
        assert_eq!(head_end(b"<head><title>t</title>", 0), None);
    }

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
