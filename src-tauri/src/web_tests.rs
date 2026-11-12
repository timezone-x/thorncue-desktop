//! The web client and web import against a tiny mock of ThornCue Web's /api/v1 (same paths,
//! JSON shapes, ETag/304 and error bodies as timezone-x/thorncue-web app.py), serving the
//! format fixtures. No real server needed.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tiny_http::{Header, Response, Server, StatusCode};

use crate::library::{Library, WebOrigin};
use crate::web::{Download, ServerUrl, WebClient, WebError, parse_join_link};

const TOKEN: &str = "tc_good-token";
const REV: &str = "18a2b3c4d5e6f";

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/format/fixtures")
        .join(rel)
}

struct Mock {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn json(status: u16, body: serde_json::Value) -> Response<Cursor<Vec<u8>>> {
    Response::from_data(body.to_string().into_bytes())
        .with_status_code(StatusCode(status))
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
}

fn error(status: u16, code: &str, message: &str) -> Response<Cursor<Vec<u8>>> {
    json(
        status,
        serde_json::json!({"error": {"code": code, "message": message}}),
    )
}

/// `thorncue = false` makes it some other server.
fn mock(thorncue: bool) -> Mock {
    let server = Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.server_addr().to_ip().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let package = std::fs::read(fixture("valid/05-from-web.tqs")).unwrap();
    let thread = std::thread::spawn(move || {
        while !stop2.load(Ordering::Acquire) {
            let Ok(Some(req)) = server.recv_timeout(std::time::Duration::from_millis(50)) else {
                continue;
            };
            let auth = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.as_str().to_string());
            let inm = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("If-None-Match"))
                .map(|h| h.value.as_str().to_string());
            let authed = auth.as_deref() == Some(&format!("Bearer {TOKEN}"));
            let path = req.url().to_string();
            let pkg = || {
                Response::from_data(package.clone())
                    .with_header(Header::from_bytes("ETag", format!("\"{REV}\"")).unwrap())
                    .with_header(
                        Header::from_bytes("Content-Type", "application/vnd.thorncue.show+zip")
                            .unwrap(),
                    )
            };
            let unauth = || error(401, "unauthorized", "A valid API token is required.");
            let r = match path.as_str() {
                "/api/v1" if thorncue => json(
                    200,
                    serde_json::json!({"api": 1, "app": "thorncue-web", "formats": {"thorncue.show": [1]}, "auth": ["token"]}),
                ),
                "/api/v1" => json(200, serde_json::json!({"app": "something-else"})),
                "/api/v1/me" if authed => json(
                    200,
                    serde_json::json!({"username": "connor", "is_admin": false}),
                ),
                "/api/v1/shows" if authed => json(
                    200,
                    serde_json::json!({"shows": [{
                        "code": "STAGE-4821", "name": "From the web", "author": "Connor", "owner": "connor",
                        "cue_count": 1, "audio_bytes": 88278, "revision": REV, "updated_at": "2026-11-12T03:10:41Z",
                        "role": "owner"
                    }]}),
                ),
                "/api/v1/shows/STAGE-4821/package" if authed => {
                    if inm.as_deref().is_some_and(|v| v.contains(REV)) {
                        Response::from_data(Vec::new())
                            .with_status_code(StatusCode(304))
                            .with_header(Header::from_bytes("ETag", format!("\"{REV}\"")).unwrap())
                    } else {
                        pkg()
                    }
                }
                "/api/v1/me" | "/api/v1/shows" | "/api/v1/shows/STAGE-4821/package" => unauth(),
                "/api/v1/links/good" => json(
                    200,
                    serde_json::json!({"code": "STAGE-4821", "name": "From the web", "author": "Connor", "owner": "connor",
                        "cue_count": 1, "audio_bytes": 88278, "revision": REV, "updated_at": "2026-11-12T03:10:41Z"}),
                ),
                "/api/v1/links/good/package" => pkg(),
                p if p.starts_with("/api/v1/links/") => error(
                    404,
                    "not_found",
                    "This link is invalid or has been revoked.",
                ),
                _ => error(404, "not_found", "Not found."),
            };
            let _ = req.respond(r);
        }
    });
    Mock {
        url,
        stop,
        thread: Some(thread),
    }
}

fn client(m: &Mock, token: Option<&str>) -> WebClient {
    WebClient::new(ServerUrl::parse(&m.url).unwrap(), token.map(String::from))
}

#[test]
fn connects_and_says_who_the_token_belongs_to() {
    let m = mock(true);
    let c = client(&m, Some(TOKEN));
    c.check_server().unwrap();
    assert_eq!(c.me().unwrap(), "connor");
}

#[test]
fn a_bad_token_reads_as_token_not_accepted() {
    let m = mock(true);
    let err = client(&m, Some("tc_wrong")).me().unwrap_err();
    assert_eq!(err, WebError::TokenRejected);
    assert!(
        err.to_string().contains("didn't accept the API token"),
        "{err}"
    );
    assert_eq!(
        client(&m, None).list_shows().unwrap_err(),
        WebError::TokenRejected
    );
}

#[test]
fn another_kind_of_server_is_refused() {
    let m = mock(false);
    assert!(matches!(
        client(&m, None).check_server(),
        Err(WebError::NotThorncue(_))
    ));
}

#[test]
fn an_unreachable_server_says_so() {
    let c = WebClient::new(ServerUrl::parse("http://127.0.0.1:9").unwrap(), None);
    assert!(matches!(c.check_server(), Err(WebError::Unreachable(_))));
}

#[test]
fn lists_shows() {
    let m = mock(true);
    let shows = client(&m, Some(TOKEN)).list_shows().unwrap();
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].code, "STAGE-4821");
    assert_eq!(shows[0].revision, REV);
}

#[test]
fn imports_a_show_then_304_when_unchanged() {
    let m = mock(true);
    let c = client(&m, Some(TOKEN));
    let data = tempfile::tempdir().unwrap();
    let lib = Library::new(data.path()).unwrap();
    let tmp = data.path().join("dl.tqs");

    let Download::Saved { revision, .. } = c
        .download_show("STAGE-4821", None, &tmp, &mut |_, _| {})
        .unwrap()
    else {
        panic!("expected a download")
    };
    assert_eq!(revision.as_deref(), Some(REV));
    let origin = WebOrigin {
        server: m.url.clone(),
        code: "STAGE-4821".into(),
        revision,
    };
    let (id, manifest, _) = lib.import_package(&tmp, &origin, &mut |_| {}).unwrap();
    assert_eq!(manifest.show.name, "From the web");

    // Recognised as imported from this server even though the manifest names its public URL.
    let found = lib.find_by_source(&m.url, "STAGE-4821");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, id);
    assert_eq!(found[0].imported_revision.as_deref(), Some(REV));
    // And by the manifest's own source too.
    assert_eq!(
        lib.find_by_source("https://cue.example.com", "STAGE-4821")
            .len(),
        1
    );

    let again = c
        .download_show(
            "STAGE-4821",
            Some(REV),
            &data.path().join("dl2.tqs"),
            &mut |_, _| {},
        )
        .unwrap();
    assert_eq!(again, Download::NotModified);
    assert!(!data.path().join("dl2.tqs").exists());
}

#[test]
fn replace_keeps_the_local_id_and_standby() {
    let m = mock(true);
    let c = client(&m, Some(TOKEN));
    let data = tempfile::tempdir().unwrap();
    let lib = Library::new(data.path()).unwrap();
    let origin = WebOrigin {
        server: m.url.clone(),
        code: "STAGE-4821".into(),
        revision: Some(REV.into()),
    };
    let tmp = data.path().join("dl.tqs");
    c.download_show("STAGE-4821", None, &tmp, &mut |_, _| {})
        .unwrap();
    let (id, _, _) = lib.import_package(&tmp, &origin, &mut |_| {}).unwrap();
    let dir = lib.show_dir(&id).unwrap();
    let mut local = crate::library::read_local(&dir);
    local.standby_index = 1;
    crate::library::write_local(&dir, &local).unwrap();

    c.download_show("STAGE-4821", None, &tmp, &mut |_, _| {})
        .unwrap();
    lib.replace_with_package(&id, &tmp, &origin, &mut |_| {})
        .unwrap();
    let shows = lib.list().unwrap();
    assert_eq!(shows.len(), 1, "replaced, not added");
    assert_eq!(shows[0].id, id);
    assert_eq!(crate::library::read_local(&dir).standby_index, 1);
    assert!(
        std::fs::read_dir(lib.root()).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.'))
    );
}

#[test]
fn a_refused_package_leaves_the_show_being_replaced_alone() {
    let data = tempfile::tempdir().unwrap();
    let lib = Library::new(data.path()).unwrap();
    let origin = WebOrigin {
        server: "https://x".into(),
        code: "C".into(),
        revision: None,
    };
    let (id, before, _) = lib
        .import_package(&fixture("valid/05-from-web.tqs"), &origin, &mut |_| {})
        .unwrap();
    assert!(
        lib.replace_with_package(
            &id,
            &fixture("invalid/sha-mismatch.tqs"),
            &origin,
            &mut |_| {}
        )
        .is_err()
    );
    assert_eq!(lib.open(&id).unwrap().manifest, before);
    assert_eq!(std::fs::read_dir(lib.root()).unwrap().count(), 1);
}

#[test]
fn a_revoked_link_is_a_404_with_the_servers_message() {
    let m = mock(true);
    let link = format!("{}/join/revoked", m.url);
    let (server, token) = parse_join_link(&link).unwrap();
    let err = WebClient::new(server, None).link_info(&token).unwrap_err();
    assert_eq!(
        err,
        WebError::NotFound("This link is invalid or has been revoked.".into())
    );
}

#[test]
fn imports_from_a_join_link() {
    let m = mock(true);
    let (server, token) = parse_join_link(&format!("{}/join/good", m.url)).unwrap();
    let c = WebClient::new(server, None);
    assert_eq!(c.link_info(&token).unwrap().code, "STAGE-4821");
    let data = tempfile::tempdir().unwrap();
    let tmp = data.path().join("dl.tqs");
    assert!(matches!(
        c.download_link(&token, &tmp, &mut |_, _| {}).unwrap(),
        Download::Saved { .. }
    ));
    let lib = Library::new(data.path()).unwrap();
    let origin = WebOrigin {
        server: m.url.clone(),
        code: "STAGE-4821".into(),
        revision: None,
    };
    lib.import_package(&tmp, &origin, &mut |_| {}).unwrap();
}

/// A server that answers one request with headers for the whole package, sends half of it,
/// then closes the connection.
fn truncating_server() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let package = std::fs::read(fixture("valid/05-from-web.tqs")).unwrap();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = sock.read(&mut buf);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.thorncue.show+zip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            package.len()
        );
        let _ = sock.write_all(head.as_bytes());
        let _ = sock.write_all(&package[..package.len() / 2]);
        let _ = sock.flush();
        // Dropping the socket hangs up mid-body.
    });
    url
}

#[test]
fn a_truncated_download_is_rejected_and_cleaned_up() {
    let url = truncating_server();
    let c = WebClient::new(ServerUrl::parse(&url).unwrap(), Some(TOKEN.into()));
    let data = tempfile::tempdir().unwrap();
    let tmp = data.path().join("dl.tqs");
    let mut last = (0, None);
    let err = c
        .download_show("STAGE-4821", None, &tmp, &mut |n, t| last = (n, t))
        .unwrap_err();
    assert_eq!(err, WebError::Truncated);
    assert!(!tmp.exists(), "the partial file is removed");
    assert!(
        last.1.is_some_and(|t| last.0 < t),
        "progress saw a short download: {last:?}"
    );
}

#[test]
fn the_token_never_appears_in_errors() {
    let m = mock(true);
    for e in [
        client(&m, Some("tc_secret-value")).me().unwrap_err(),
        client(&m, Some("tc_secret-value"))
            .list_shows()
            .unwrap_err(),
    ] {
        assert!(!e.to_string().contains("secret-value"));
    }
}
