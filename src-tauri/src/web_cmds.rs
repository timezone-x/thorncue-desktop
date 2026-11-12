//! Tauri commands for ThornCue Web (docs/ARCHITECTURE.md "Importing from ThornCue Web").
//! Network work runs on blocking tasks; download and unpack progress goes out as
//! `import://progress` with `path` = "web:<code>".

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::AppState;
use crate::commands::{ImportProgress, ImportResult};
use crate::library::{ShowSummary, WebOrigin};
use crate::web::{self, Download, ServerUrl, TokenStore, WebClient, WebShow};

fn busy() -> String {
    "ThornCue is busy. Try again.".into()
}

/// The connection as the Settings screen shows it. Never includes the token.
#[derive(Debug, Clone, Serialize)]
pub struct WebConnection {
    pub server: Option<String>,
    pub insecure: bool,
    pub has_token: bool,
    pub token_store: Option<TokenStore>,
    /// Set by a successful test.
    pub username: Option<String>,
}

fn token_for(state: &AppState, server: &str) -> Option<(String, TokenStore)> {
    if let Some(t) = web::keychain_load(server) {
        return Some((t, TokenStore::Keychain));
    }
    let s = state.settings.lock().ok()?;
    match (&s.web_server, &s.web_token_fallback) {
        (Some(srv), Some(t)) if srv == server => Some((t.clone(), TokenStore::SettingsFile)),
        _ => None,
    }
}

fn connection(state: &AppState, username: Option<String>) -> Result<WebConnection, String> {
    let server = state
        .settings
        .lock()
        .map_err(|_| busy())?
        .web_server
        .clone();
    let token = server.as_deref().and_then(|s| token_for(state, s));
    Ok(WebConnection {
        insecure: server.as_deref().is_some_and(|s| s.starts_with("http://")),
        has_token: token.is_some(),
        token_store: token.map(|(_, store)| store),
        server,
        username,
    })
}

/// The configured client, or a sentence saying what to set up.
fn client(state: &AppState) -> Result<WebClient, String> {
    let server = state
        .settings
        .lock()
        .map_err(|_| busy())?
        .web_server
        .clone()
        .ok_or("Connect to ThornCue Web in Settings first.")?;
    let (token, _) =
        token_for(state, &server).ok_or("Add your ThornCue Web API token in Settings first.")?;
    let url = ServerUrl::parse(&server).map_err(|e| e.to_string())?;
    Ok(WebClient::new(url, Some(token)))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| {
        tracing::error!("web task failed: {e}");
        "That stopped unexpectedly. Try again.".to_string()
    })?
}

#[tauri::command]
pub fn web_connection(state: State<'_, AppState>) -> Result<WebConnection, String> {
    connection(&state, None)
}

/// Check the URL is a ThornCue server and the token works, then save both: the URL in
/// settings.json, the token in the OS keychain (or settings.json, 0600, if there isn't one).
#[tauri::command]
pub async fn web_connect(
    server: String,
    token: String,
    app: AppHandle,
) -> Result<WebConnection, String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let url = ServerUrl::parse(&server).map_err(|e| e.to_string())?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return Err("Paste the API token from the web's Settings page.".into());
        }
        let c = WebClient::new(url.clone(), Some(token.clone()));
        c.check_server().map_err(|e| e.to_string())?;
        let username = c.me().map_err(|e| e.to_string())?;

        let in_keychain = web::keychain_save(&url.url, &token);
        let mut s = state.settings.lock().map_err(|_| busy())?;
        let mut next = s.clone();
        if let Some(old) = &next.web_server
            && old != &url.url
        {
            web::keychain_delete(old);
        }
        next.web_server = Some(url.url.clone());
        next.web_token_fallback = (!in_keychain).then_some(token);
        next.save(&state.settings_path).map_err(|e| {
            tracing::error!("saving settings: {e:#}");
            "Couldn't save the connection. Check there is free disk space.".to_string()
        })?;
        *s = next;
        drop(s);
        tracing::info!("connected to {} as {username}", url.url);
        connection(&state, Some(username))
    })
    .await
}

/// "Connected as …": checks the saved token still works.
#[tauri::command]
pub async fn web_check(app: AppHandle) -> Result<WebConnection, String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let username = client(&state)?.me().map_err(|e| e.to_string())?;
        connection(&state, Some(username))
    })
    .await
}

/// Forget the token (the server address stays filled in).
#[tauri::command]
pub fn web_disconnect(state: State<'_, AppState>) -> Result<WebConnection, String> {
    let mut s = state.settings.lock().map_err(|_| busy())?;
    if let Some(server) = &s.web_server {
        web::keychain_delete(server);
    }
    let mut next = s.clone();
    next.web_token_fallback = None;
    next.save(&state.settings_path).map_err(|e| {
        tracing::error!("saving settings: {e:#}");
        "Couldn't save your settings. Check there is free disk space.".to_string()
    })?;
    *s = next;
    drop(s);
    connection(&state, None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalStatus {
    NotImported,
    Imported,
    UpdateAvailable,
    /// The web copy changed and so did this one.
    ChangedBoth,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebShowRow {
    #[serde(flatten)]
    pub show: WebShow,
    pub status: LocalStatus,
    /// The library show it matches (the most recently opened, if several).
    pub local_id: Option<String>,
    pub local_count: usize,
}

fn status_of(web: &WebShow, local: &[ShowSummary]) -> (LocalStatus, Option<String>) {
    let Some(first) = local.first() else {
        return (LocalStatus::NotImported, None);
    };
    let id = Some(first.id.clone());
    if first.imported_revision.as_deref() == Some(web.revision.as_str()) {
        (LocalStatus::Imported, id)
    } else if first.modified_since_import {
        (LocalStatus::ChangedBoth, id)
    } else {
        (LocalStatus::UpdateAvailable, id)
    }
}

#[tauri::command]
pub async fn web_list(app: AppHandle) -> Result<Vec<WebShowRow>, String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let c = client(&state)?;
        let shows = c.list_shows().map_err(|e| e.to_string())?;
        let server = c.server().url.clone();
        Ok(shows
            .into_iter()
            .map(|show| {
                let local = state.library.find_by_source(&server, &show.code);
                let (status, local_id) = status_of(&show, &local);
                WebShowRow {
                    show,
                    status,
                    local_id,
                    local_count: local.len(),
                }
            })
            .collect())
    })
    .await
}

/// What a share link points at, and whether it's already in the library.
#[derive(Debug, Clone, Serialize)]
pub struct LinkInfo {
    pub server: String,
    pub insecure: bool,
    #[serde(flatten)]
    pub show: WebShow,
    pub status: LocalStatus,
    pub local_id: Option<String>,
}

#[tauri::command]
pub async fn web_link_info(link: String, app: AppHandle) -> Result<LinkInfo, String> {
    blocking(move || {
        let state = app.state::<AppState>();
        let (server, token) = web::parse_join_link(&link).map_err(|e| e.to_string())?;
        let c = WebClient::new(server.clone(), None);
        c.check_server().map_err(|e| e.to_string())?;
        let show = c.link_info(&token).map_err(|e| e.to_string())?;
        let local = state.library.find_by_source(&server.url, &show.code);
        let (status, local_id) = status_of(&show, &local);
        Ok(LinkInfo {
            server: server.url,
            insecure: server.insecure,
            show,
            status,
            local_id,
        })
    })
    .await
}

/// Which web show to import.
enum Source {
    Show { code: String },
    Link { link: String },
}

/// Download, validate and import. `replace_id`: swap that library show's content (Replace);
/// `None`: add a new show (first import, or Keep both).
fn import(
    app: &AppHandle,
    source: Source,
    replace_id: Option<String>,
) -> Result<ImportResult, String> {
    let state = app.state::<AppState>();
    if let Some(id) = &replace_id {
        let open = state.open_show.lock().map_err(|_| busy())?;
        if open.as_ref().is_some_and(|s| &s.id == id) {
            return Err("Close this show before replacing it.".into());
        }
    }
    let (c, key, code, link_token) = match &source {
        Source::Show { code } => (client(&state)?, format!("web:{code}"), code.clone(), None),
        Source::Link { link } => {
            let (server, token) = web::parse_join_link(link).map_err(|e| e.to_string())?;
            let c = WebClient::new(server, None);
            let code = c.link_info(&token).map_err(|e| e.to_string())?.code;
            (c, format!("web:{code}"), code, Some(token))
        }
    };
    // Replacing an up-to-date copy: ask for 304 and skip the download.
    let if_none_match = replace_id.as_deref().and_then(|id| {
        state
            .library
            .list()
            .ok()?
            .into_iter()
            .find(|s| s.id == id)?
            .imported_revision
    });

    std::fs::create_dir_all(&state.tmp_dir)
        .map_err(|e| format!("Couldn't prepare the download folder: {e}."))?;
    let tmp = tempfile::Builder::new()
        .prefix("web-")
        .suffix(".tqs")
        .tempfile_in(&state.tmp_dir)
        .map_err(|e| format!("Couldn't prepare the download: {e}."))?
        .into_temp_path();

    let emit = |done: u64, total: u64| {
        let _ = app.emit(
            "import://progress",
            ImportProgress {
                path: key.clone(),
                done,
                total,
            },
        );
    };
    let mut last = std::time::Instant::now();
    let mut on_download = |n: u64, total: Option<u64>| {
        if last.elapsed().as_millis() >= 50 || Some(n) == total {
            last = std::time::Instant::now();
            emit(n, total.unwrap_or(0));
        }
    };
    let downloaded = match &link_token {
        None => c.download_show(&code, if_none_match.as_deref(), &tmp, &mut on_download),
        Some(t) => c.download_link(t, &tmp, &mut on_download),
    }
    .map_err(|e| e.to_string())?;
    let revision = match downloaded {
        Download::NotModified => {
            return Ok(ImportResult {
                id: replace_id.unwrap_or_default(),
                name: String::new(),
                cue_count: 0,
                warnings: vec!["This show was already up to date.".into()],
            });
        }
        Download::Saved { revision, .. } => revision,
    };
    let origin = WebOrigin {
        server: c.server().url.clone(),
        code: code.clone(),
        revision,
    };
    let mut on_unpack = |p: tqs::Progress| emit(p.done, p.total);
    let result = match replace_id {
        Some(id) => state
            .library
            .replace_with_package(&id, &tmp, &origin, &mut on_unpack)
            .map(|(m, w)| (id, m, w))
            .map_err(|e| e.to_string()),
        None => state
            .library
            .import_package(&tmp, &origin, &mut on_unpack)
            .map_err(|e| e.to_string()),
    };
    drop(tmp); // removes the download
    let (id, m, warnings) = result?;
    tracing::info!("imported {code} from {} as {id}", origin.server);
    Ok(ImportResult {
        id,
        name: m.show.name,
        cue_count: m.cues.len(),
        warnings: warnings.into_iter().map(|w| w.0).collect(),
    })
}

#[tauri::command]
pub async fn web_import(
    code: String,
    replace_id: Option<String>,
    app: AppHandle,
) -> Result<ImportResult, String> {
    blocking(move || import(&app, Source::Show { code }, replace_id)).await
}

#[tauri::command]
pub async fn web_import_link(
    link: String,
    replace_id: Option<String>,
    app: AppHandle,
) -> Result<ImportResult, String> {
    blocking(move || import(&app, Source::Link { link }, replace_id)).await
}
