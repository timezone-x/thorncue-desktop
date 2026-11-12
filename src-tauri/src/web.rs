//! Client for ThornCue Web's read-only API (docs/ARCHITECTURE.md "Importing from ThornCue
//! Web"; endpoints as in timezone-x/thorncue-web app.py "/api/v1").
//!
//! Blocking: callers run it on a blocking task. The token goes only in the Authorization
//! header: never in a URL, never in a log line, never back to the webview.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Everything that can go wrong, as a sentence for the operator.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebError {
    #[error("Couldn't reach {0}. Check the address and that this computer is online.")]
    Unreachable(String),
    #[error("{0} isn't a ThornCue Web server, or it's a version this app can't import from.")]
    NotThorncue(String),
    #[error(
        "The server didn't accept the API token. Create a new token on the web's Settings page and paste it here."
    )]
    TokenRejected,
    #[error("{0}")]
    NotFound(String),
    #[error("The server is busy. {0}")]
    RateLimited(String),
    #[error("The server said: {0}")]
    Server(String),
    #[error("The download was cut short. Check the connection and try again.")]
    Truncated,
    #[error("That isn't a web address. It should look like https://cues.example.com.")]
    BadUrl,
    #[error("That isn't a ThornCue join link. It should look like https://cues.example.com/join/…")]
    BadLink,
    #[error("Couldn't save the download: {0}. Check there is free disk space.")]
    Disk(String),
}

/// What `GET /api/v1` says.
#[derive(Debug, Clone, Deserialize)]
struct ApiRoot {
    app: String,
    #[serde(default)]
    formats: std::collections::HashMap<String, Vec<u32>>,
}

#[derive(Debug, Clone, Deserialize)]
struct Me {
    username: String,
}

/// One show as the web lists it (utils.show_summary + role).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebShow {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub cue_count: u64,
    #[serde(default)]
    pub audio_bytes: u64,
    pub revision: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
struct ShowList {
    shows: Vec<WebShow>,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ErrorDetail {
    #[serde(default)]
    message: String,
}

/// Result of a package download.
#[derive(Debug, PartialEq, Eq)]
pub enum Download {
    /// 304: the local copy is already this revision.
    NotModified,
    /// Saved; `revision` is the response's ETag (the web's revision for this show).
    Saved {
        bytes: u64,
        revision: Option<String>,
    },
}

/// A server address the operator typed: scheme + host (+ port), no trailing slash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerUrl {
    pub url: String,
    /// Plain http: the token travels unencrypted. Allowed for LAN installs, with a warning.
    pub insecure: bool,
}

impl ServerUrl {
    pub fn parse(input: &str) -> Result<Self, WebError> {
        let mut s = input.trim().to_string();
        if s.is_empty() {
            return Err(WebError::BadUrl);
        }
        if !s.contains("://") {
            s = format!("https://{s}");
        }
        let u = reqwest::Url::parse(&s).map_err(|_| WebError::BadUrl)?;
        if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
            return Err(WebError::BadUrl);
        }
        Ok(Self {
            url: origin(&u),
            insecure: u.scheme() == "http",
        })
    }

    /// Two spellings of the same server compare equal (case, default port, trailing slash).
    pub fn same_server(a: &str, b: &str) -> bool {
        match (Self::parse(a), Self::parse(b)) {
            (Ok(x), Ok(y)) => x.url == y.url,
            _ => false,
        }
    }
}

fn origin(u: &reqwest::Url) -> String {
    let host = u.host_str().unwrap_or("").to_lowercase();
    match u.port() {
        Some(p) => format!("{}://{host}:{p}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    }
}

/// A pasted share link: `https://host/join/<token>` → (server, token).
pub fn parse_join_link(input: &str) -> Result<(ServerUrl, String), WebError> {
    let s = input.trim();
    let u = reqwest::Url::parse(s).map_err(|_| WebError::BadLink)?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(WebError::BadLink);
    }
    let segs: Vec<&str> = u
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    let token = match segs.as_slice() {
        [.., "join", token] => *token,
        _ => return Err(WebError::BadLink),
    };
    if token.is_empty()
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(WebError::BadLink);
    }
    Ok((
        ServerUrl {
            url: origin(&u),
            insecure: u.scheme() == "http",
        },
        token.to_string(),
    ))
}

pub struct WebClient {
    server: ServerUrl,
    token: Option<String>,
    http: reqwest::blocking::Client,
}

impl WebClient {
    pub fn new(server: ServerUrl, token: Option<String>) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("ThornCue-desktop/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            // In the blocking client this limit applies to each read, not the whole download
            // (reqwest blocking/response.rs): a big show on slow venue Wi-Fi takes as long as
            // it takes, but a server that stops sending for 30 s has failed.
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("building the HTTP client");
        Self {
            server,
            token,
            http,
        }
    }

    pub fn server(&self) -> &ServerUrl {
        &self.server
    }

    fn get(&self, path: &str, auth: bool) -> reqwest::blocking::RequestBuilder {
        let mut req = self.http.get(format!("{}{path}", self.server.url));
        if auth && let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        req
    }

    fn send(
        &self,
        req: reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::Response, WebError> {
        let resp = req.send().map_err(|e| {
            // reqwest errors can carry the URL but never the Authorization header.
            tracing::warn!("request to {} failed: {e}", self.server.url);
            WebError::Unreachable(self.server.url.clone())
        })?;
        let status = resp.status();
        if status.is_success() || status == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(resp);
        }
        let retry = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let message = resp
            .text()
            .ok()
            .and_then(|t| serde_json::from_str::<ErrorBody>(&t).ok())
            .map(|b| b.error.message)
            .filter(|m| !m.is_empty());
        tracing::warn!("{} answered {status}", self.server.url);
        Err(match status.as_u16() {
            401 => WebError::TokenRejected,
            404 => WebError::NotFound(
                message.unwrap_or_else(|| "The server doesn't have that show.".into()),
            ),
            429 => WebError::RateLimited(match retry {
                Some(s) => format!("Try again in {s} seconds."),
                None => "Wait a minute and try again.".into(),
            }),
            _ => WebError::Server(message.unwrap_or_else(|| format!("error {status}"))),
        })
    }

    fn json<T: for<'de> Deserialize<'de>>(&self, path: &str, auth: bool) -> Result<T, WebError> {
        let resp = self.send(self.get(path, auth))?;
        let body = resp
            .bytes()
            .map_err(|_| WebError::Unreachable(self.server.url.clone()))?;
        serde_json::from_slice::<T>(&body).map_err(|e| {
            tracing::warn!("unexpected reply from {}{path}: {e}", self.server.url);
            WebError::NotThorncue(self.server.url.clone())
        })
    }

    /// Is this a ThornCue Web server that serves format 1 packages?
    pub fn check_server(&self) -> Result<(), WebError> {
        let root: ApiRoot = self.json("/api/v1", false)?;
        let ok = root.app == "thorncue-web"
            && root
                .formats
                .get("thorncue.show")
                .is_some_and(|v| v.contains(&1));
        if ok {
            Ok(())
        } else {
            Err(WebError::NotThorncue(self.server.url.clone()))
        }
    }

    /// Who the token belongs to.
    pub fn me(&self) -> Result<String, WebError> {
        Ok(self.json::<Me>("/api/v1/me", true)?.username)
    }

    pub fn list_shows(&self) -> Result<Vec<WebShow>, WebError> {
        Ok(self.json::<ShowList>("/api/v1/shows", true)?.shows)
    }

    /// What a share link points at (no token needed).
    pub fn link_info(&self, link_token: &str) -> Result<WebShow, WebError> {
        self.json(&format!("/api/v1/links/{link_token}"), false)
    }

    pub fn download_show(
        &self,
        code: &str,
        if_none_match: Option<&str>,
        dest: &Path,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Download, WebError> {
        let code = urlencode(code);
        self.download(
            &format!("/api/v1/shows/{code}/package"),
            true,
            if_none_match,
            dest,
            on_progress,
        )
    }

    pub fn download_link(
        &self,
        link_token: &str,
        dest: &Path,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Download, WebError> {
        self.download(
            &format!("/api/v1/links/{link_token}/package"),
            false,
            None,
            dest,
            on_progress,
        )
    }

    /// Stream a package into `dest`. On any failure `dest` is removed.
    fn download(
        &self,
        path: &str,
        auth: bool,
        if_none_match: Option<&str>,
        dest: &Path,
        on_progress: &mut dyn FnMut(u64, Option<u64>),
    ) -> Result<Download, WebError> {
        let mut req = self.get(path, auth);
        if let Some(rev) = if_none_match {
            req = req.header(reqwest::header::IF_NONE_MATCH, format!("\"{rev}\""));
        }
        let mut resp = self.send(req)?;
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(Download::NotModified);
        }
        let total = resp.content_length();
        let revision = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_start_matches("W/").trim_matches('"').to_string())
            .filter(|v| !v.is_empty());
        let result = (|| {
            let mut out = File::create(dest).map_err(|e| WebError::Disk(e.to_string()))?;
            let mut buf = vec![0u8; 256 * 1024];
            let mut n: u64 = 0;
            on_progress(0, total);
            loop {
                let got = match resp.read(&mut buf) {
                    Ok(0) => break,
                    Ok(g) => g,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        tracing::warn!(
                            "download from {} broke off after {n} bytes: {e}",
                            self.server.url
                        );
                        return Err(WebError::Truncated);
                    }
                };
                out.write_all(&buf[..got])
                    .map_err(|e| WebError::Disk(e.to_string()))?;
                n += got as u64;
                on_progress(n, total);
            }
            if total.is_some_and(|t| t != n) {
                return Err(WebError::Truncated);
            }
            out.sync_all().map_err(|e| WebError::Disk(e.to_string()))?;
            Ok(Download::Saved { bytes: n, revision })
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(dest);
        }
        result
    }
}

/// Show codes are `[A-Z0-9-]`, but never trust that in a URL path.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The API token: OS keychain, or settings.json when there isn't one.
// ---------------------------------------------------------------------------

const KEYRING_SERVICE: &str = "thorncue";

/// Where the token ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenStore {
    Keychain,
    /// No keychain (some Linux setups): kept in settings.json, readable only by this user.
    SettingsFile,
}

pub fn keychain_save(server: &str, token: &str) -> bool {
    match keyring::Entry::new(KEYRING_SERVICE, server).and_then(|e| e.set_password(token)) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("keychain unavailable, keeping the token in settings.json: {e}");
            false
        }
    }
}

pub fn keychain_load(server: &str) -> Option<String> {
    keyring::Entry::new(KEYRING_SERVICE, server)
        .and_then(|e| e.get_password())
        .ok()
}

pub fn keychain_delete(server: &str) {
    if let Ok(e) = keyring::Entry::new(KEYRING_SERVICE, server) {
        let _ = e.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls_normalise() {
        let s = ServerUrl::parse(" HTTPS://Cues.Example.com/ ").unwrap();
        assert_eq!(s.url, "https://cues.example.com");
        assert!(!s.insecure);
        assert_eq!(
            ServerUrl::parse("cues.example.com").unwrap().url,
            "https://cues.example.com"
        );
        let lan = ServerUrl::parse("http://192.168.1.20:8000/x").unwrap();
        assert_eq!(lan.url, "http://192.168.1.20:8000");
        assert!(lan.insecure);
        assert!(ServerUrl::parse("ftp://x").is_err());
        assert!(ServerUrl::parse("").is_err());
        assert!(ServerUrl::same_server(
            "https://a.example/",
            "HTTPS://a.example:443"
        ));
    }

    #[test]
    fn join_links_parse() {
        let (s, t) = parse_join_link("https://cues.example.com/join/AbC_d-9").unwrap();
        assert_eq!(s.url, "https://cues.example.com");
        assert_eq!(t, "AbC_d-9");
        assert!(parse_join_link("https://cues.example.com/show/X").is_err());
        assert!(parse_join_link("https://cues.example.com/join/a%2Fb").is_err());
        assert!(parse_join_link("not a url").is_err());
    }
}
