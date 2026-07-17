//! yedit's loopback server: the viewport page + its same-origin API, and the
//! libyggterm sidebar control endpoint (`GET /pane/notes`, `POST /action`).
//!
//! Hand-rolled HTTP over `TcpListener` (the ychrome pattern): one tiny request
//! shape, no framework. The GUI reaches `/pane/*` and `/action` over a plain
//! socket; the page reaches `/api/*` same-origin from the surface webview.

use crate::docs::{SaveOutcome, Store};
use crate::render;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct Server {
    pub url: String,
    pub state: Arc<Mutex<Store>>,
}

pub fn spawn(store: Store) -> Result<Server> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding yedit server")?;
    let port = listener.local_addr()?.port();
    let state = Arc::new(Mutex::new(store));
    {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(stream) = incoming else { continue };
                let state = Arc::clone(&state);
                std::thread::spawn(move || handle_conn(stream, &state));
            }
        });
    }
    Ok(Server {
        url: format!("http://127.0.0.1:{port}"),
        state,
    })
}

fn handle_conn(stream: TcpStream, state: &Mutex<Store>) {
    let Ok(peek) = stream.try_clone() else { return };
    let mut reader = BufReader::new(peek);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let body: Value = if content_length > 0 {
        let mut raw = vec![0u8; content_length];
        if reader.read_exact(&mut raw).is_err() {
            return;
        }
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    } else {
        Value::Null
    };

    match (method, path) {
        ("GET", "/") => respond_html(stream, render::page_html()),
        ("GET", "/api/state") => {
            let store = state.lock().unwrap();
            respond_json(stream, 200, &state_json(&store));
        }
        ("GET", "/api/doc") => {
            let id = query_value(query, "id").unwrap_or_default();
            let mut store = state.lock().unwrap();
            if query_value(query, "reload").is_some() {
                let _ = store.reload(&id);
            }
            match store.get(&id) {
                Some(note) => {
                    let payload = json!({
                        "id": note.id,
                        "name": note.name(),
                        "path": note.path.to_string_lossy(),
                        "content": note.content,
                        "html": render::markdown_to_html(&note.content),
                        "dirty": note.dirty,
                    });
                    respond_json(stream, 200, &payload);
                }
                None => respond_json(stream, 404, &json!({ "error": "unknown note" })),
            }
        }
        ("POST", "/api/edit") => {
            let id = body["id"].as_str().unwrap_or_default().to_string();
            let content = body["content"].as_str().unwrap_or_default().to_string();
            let mut store = state.lock().unwrap();
            store.edit(&id, content);
            respond_json(stream, 200, &json!({ "ok": true }));
        }
        ("POST", "/api/save") => {
            let id = body["id"].as_str().unwrap_or_default().to_string();
            let content = body["content"].as_str().unwrap_or_default().to_string();
            let force = body["force"].as_bool().unwrap_or(false);
            let mut store = state.lock().unwrap();
            match store.save(&id, content, force) {
                Ok(SaveOutcome::Saved { revision }) => {
                    respond_json(stream, 200, &json!({ "ok": true, "revision": revision }));
                }
                Ok(SaveOutcome::Conflict { disk, loaded }) => {
                    respond_json(
                        stream,
                        409,
                        &json!({ "conflict": true, "disk": disk, "loaded": loaded }),
                    );
                }
                Err(error) => {
                    respond_json(stream, 500, &json!({ "error": error.to_string() }));
                }
            }
        }
        ("POST", "/api/reload") => {
            let id = body["id"].as_str().unwrap_or_default().to_string();
            let mut store = state.lock().unwrap();
            match store.reload(&id) {
                Ok(()) => respond_json(stream, 200, &json!({ "ok": true })),
                Err(error) => respond_json(stream, 500, &json!({ "error": error.to_string() })),
            }
        }
        ("POST", "/api/open") => {
            let path = body["path"].as_str().unwrap_or_default().to_string();
            let mut store = state.lock().unwrap();
            match store.open(Path::new(&path)) {
                Ok(id) => respond_json(stream, 200, &json!({ "ok": true, "id": id })),
                Err(error) => respond_json(stream, 500, &json!({ "error": error.to_string() })),
            }
        }
        ("POST", "/api/mode") => {
            let markdown = body["markdown"].as_bool().unwrap_or(true);
            let mut store = state.lock().unwrap();
            store.markdown_mode = markdown;
            store.touch();
            respond_json(stream, 200, &json!({ "markdown_mode": store.markdown_mode }));
        }
        ("GET", "/pane/notes") => {
            let store = state.lock().unwrap();
            respond_json(stream, 200, &pane_schema(&store, None));
        }
        ("POST", "/action") => {
            let reply = handle_pane_action(state, &body);
            respond_json(stream, 200, &reply);
        }
        _ => respond_json(stream, 404, &json!({})),
    }
}

fn state_json(store: &Store) -> Value {
    json!({
        "active_id": store.active_id,
        "markdown_mode": store.markdown_mode,
        "epoch": store.epoch,
        "notes": store.notes.iter().map(|n| json!({
            "id": n.id, "name": n.name(), "dirty": n.dirty,
        })).collect::<Vec<_>>(),
        "recent": store.recent.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
    })
}

/// The contributed Notes pane: vertical note tabs (open = switch, ✕ = close),
/// an open-or-create path box, and recent files. This IS yedit's v1 file
/// picker; the module boundary is the extraction seam for the shared
/// libyggterm picker component once a second app needs one
/// (extraction-not-construction).
fn pane_schema(store: &Store, draft_path: Option<&str>) -> Value {
    let mut widgets = vec![json!({ "kind": "section", "text": "Open notes" })];
    if store.notes.is_empty() {
        widgets.push(json!({
            "kind": "label", "muted": true,
            "text": "No notes open. Open one below.",
        }));
    }
    for note in &store.notes {
        let active = store.active_id.as_deref() == Some(note.id.as_str());
        let mut title = note.name();
        if note.dirty {
            title = format!("● {title}");
        }
        if active {
            title = format!("▸ {title}");
        }
        widgets.push(json!({
            "kind": "list-row",
            "id": note.id,
            "title": title,
            "subtitle": note.path.to_string_lossy(),
            "actions": [
                { "action": "open_note", "label": "⤢", "title": "Show this note in the viewport" },
                { "action": "close_note", "label": "✕", "title": "Close this note (unsaved edits are kept until yedit exits)" },
            ],
        }));
    }
    widgets.push(json!({ "kind": "section", "text": "Open or create" }));
    widgets.push(json!({
        "kind": "text-input", "id": "open_path",
        "label": "Path", "placeholder": "~/notes/todo.md",
        "value": draft_path.unwrap_or(""),
        "action": "open_path",
    }));
    widgets.push(json!({
        "kind": "button", "id": "open_btn", "label": "Open",
        "action": "open_path", "primary": true,
    }));
    if !store.recent.is_empty() {
        widgets.push(json!({ "kind": "section", "text": "Recent" }));
        for path in store.recent.iter().take(8) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            widgets.push(json!({
                "kind": "list-row",
                "id": format!("recent-{}", crate::docs::note_id(path)),
                "title": name,
                "subtitle": path.to_string_lossy(),
                "actions": [
                    { "action": "open_recent", "label": "⤢", "title": "Open this file" },
                ],
            }));
        }
    }
    json!({ "title": "Yedit notes", "widgets": widgets })
}

fn handle_pane_action(state: &Mutex<Store>, body: &Value) -> Value {
    let action = body["action"].as_str().unwrap_or_default();
    let row = body["row"].as_str().unwrap_or_default();
    let values = &body["values"];
    let mut store = state.lock().unwrap();
    let mut toast: Option<String> = None;
    let mut draft: Option<String> = None;
    match action {
        "open_note" => {
            if store.notes.iter().any(|n| n.id == row) {
                store.active_id = Some(row.to_string());
                store.touch();
            }
        }
        "close_note" => {
            store.close(row);
        }
        "open_path" => {
            let raw = values["open_path"].as_str().unwrap_or_default().trim().to_string();
            if raw.is_empty() {
                toast = Some("Enter a path to open".to_string());
            } else {
                match store.open(Path::new(&raw)) {
                    Ok(_) => toast = Some(format!("Opened {raw}")),
                    Err(error) => {
                        toast = Some(format!("Open failed: {error}"));
                        draft = Some(raw);
                    }
                }
            }
        }
        "open_recent" => {
            let recent: Option<PathBuf> = store
                .recent
                .iter()
                .find(|p| format!("recent-{}", crate::docs::note_id(p)) == row)
                .cloned();
            if let Some(path) = recent {
                if let Err(error) = store.open(&path) {
                    toast = Some(format!("Open failed: {error}"));
                }
            }
        }
        _ => toast = Some(format!("Unknown action {action}")),
    }
    let mut reply = json!({
        "schema": pane_schema(&store, draft.as_deref()),
        // Nudge the page off its poll interval so a pane click lands at once.
        "eval": "window.yeditPoll && window.yeditPoll()",
    });
    if let Some(toast) = toast {
        reply["toast"] = Value::String(toast);
    }
    reply
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn respond_json(mut stream: TcpStream, status: u16, value: &Value) {
    let body = value.to_string();
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len(),
    );
    let _ = stream.flush();
}

fn respond_html(mut stream: TcpStream, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len(),
    );
    let _ = stream.flush();
}
