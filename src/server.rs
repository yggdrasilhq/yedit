//! yedit's control endpoint: `GET /pane/doc` (the document schema) and
//! `POST /action` (everything the user does in it). Hand-rolled HTTP over
//! `TcpListener` (the ychrome pattern): one tiny request shape, no framework.
//!
//! yedit renders NOTHING itself — the schema declares widgets and yggterm
//! paints them as shell DOM. The editor draft rides `values.editor` on every
//! action POST (the GUI sends the pane's declared values back), so any action
//! — save, mode toggle, tab switch — flushes the user's edits up first.

use crate::docs::{note_id, SaveOutcome, Store};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The store plus pane-local UI state (the pending save conflict).
pub struct PaneState {
    pub store: Store,
    /// Set when a save hit the revision guard; the schema then shows the
    /// Overwrite / Reload choice until the user picks one.
    pub conflict: Option<String>,
    /// The sidebar's tab filter (the search box). Empty = show every note.
    /// Compiled as a regex when it parses; substring match otherwise.
    pub search: String,
    /// Whether the open-or-create path input is shown (the 📂 toolbar
    /// button toggles it — keeps the resting sidebar to the essentials).
    pub open_input: bool,
    /// The note id currently being renamed (via the row's right-click menu).
    /// While set, the sidebar shows a rename field prefilled with its name.
    pub renaming: Option<String>,
}

pub struct Server {
    pub url: String,
    pub state: Arc<Mutex<PaneState>>,
}

pub fn spawn(store: Store) -> Result<Server> {
    let listener = TcpListener::bind("127.0.0.1:0").context("binding yedit control server")?;
    let port = listener.local_addr()?.port();
    let state = Arc::new(Mutex::new(PaneState {
        store,
        conflict: None,
        search: String::new(),
        open_input: false,
        renaming: None,
    }));
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

fn handle_conn(stream: TcpStream, state: &Mutex<PaneState>) {
    let Ok(peek) = stream.try_clone() else { return };
    let mut reader = BufReader::new(peek);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, _query) = target.split_once('?').unwrap_or((target, ""));

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
        ("POST", "/open") => {
            // The thin client's verb (Phase 4 daemon split): route a file
            // into this daemon and return. The CLIENT resolved the path
            // against its own cwd — the daemon's cwd is meaningless for it.
            let raw = body["path"].as_str().unwrap_or_default();
            let mut pane = state.lock().unwrap();
            if raw.is_empty() {
                // No file: just ensure the surface has something to show
                // (session restore already ran at daemon start).
                respond_json(
                    stream,
                    200,
                    &json!({ "ok": true, "document_version": version_of(&pane) }),
                );
                return;
            }
            match pane.store.open(Path::new(raw)) {
                Ok(id) => {
                    let version = version_of(&pane);
                    respond_json(
                        stream,
                        200,
                        &json!({ "ok": true, "id": id, "document_version": version }),
                    );
                }
                Err(error) => {
                    respond_json(stream, 200, &json!({ "ok": false, "error": error.to_string() }));
                }
            }
        }
        ("GET", "/ping") => {
            // Endpoint-ping liveness (libyggterm Phase 2): answering IS the
            // proof of life — a suspended yedit stops answering, a detached
            // one keeps its surface alive without a PTY client. The stamp
            // rides along so a GUI that no longer reads declares still
            // notices content changes and refetches.
            respond_json(
                stream,
                200,
                &json!({
                    "ok": true,
                    "app_name": "Yedit",
                    "document_version": document_version(state),
                }),
            );
        }
        ("GET", "/pane/doc") => {
            let pane = state.lock().unwrap();
            respond_json(stream, 200, &document_schema(&pane));
        }
        ("GET", "/pane/notes") => {
            let pane = state.lock().unwrap();
            respond_json(stream, 200, &notes_schema(&pane));
        }
        ("POST", "/action") => {
            let reply = handle_action(state, &body);
            respond_json(stream, 200, &reply);
        }
        _ => respond_json(stream, 404, &json!({})),
    }
}

/// The VIEWPORT pane: the document body ONLY (user direction 2026-07-17 —
/// "the viewport should be just the text editor"). Rendered markdown in
/// markdown mode, the line-numbered plain editor otherwise; the recent files
/// when nothing is open. Every control lives in the sidebar pane.
fn document_schema(pane: &PaneState) -> Value {
    use crate::docs::ViewMode;
    let store = &pane.store;
    let mut widgets: Vec<Value> = Vec::new();
    match store.active() {
        // The tri-slider's three states (libyggterm Phase 4):
        // Markdown = pure rendered reader; Split = editor + per-keystroke
        // live preview (live_from, no round trip); Text = plain editor.
        Some(note) if store.view_mode == ViewMode::Markdown => {
            widgets.push(json!({
                "kind": "markdown", "id": "body", "source": note.content,
            }));
        }
        Some(note) if store.view_mode == ViewMode::Split => {
            widgets.push(json!({
                "kind": "text-input", "id": "editor", "multiline": true,
                "line_numbers": true, "word_wrap": store.word_wrap,
                "value": note.content,
            }));
            widgets.push(json!({
                "kind": "markdown", "id": "body", "source": note.content,
                "live_from": "editor",
            }));
        }
        Some(note) => {
            widgets.push(json!({
                "kind": "text-input", "id": "editor", "multiline": true,
                "line_numbers": true, "word_wrap": store.word_wrap,
                "value": note.content,
            }));
        }
        None => {
            for path in store.recent.iter().take(10) {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                widgets.push(json!({
                    "kind": "list-row",
                    "id": format!("recent-{}", note_id(path)),
                    "title": name,
                    "subtitle": path.to_string_lossy(),
                    "actions": [
                        { "action": "open_recent", "label": "⤢", "title": "Open this file" },
                    ],
                }));
            }
        }
    }
    json!({ "title": "Yedit", "widgets": widgets })
}

/// The SIDEBAR pane (yggterm auto-opens it with the document). Top to
/// bottom, per the user's spec (2026-07-17): quick-actions toolbar (the
/// MS-Office strip), the regex search over every note's name AND content,
/// the markdown/text slider, the "Files +" heading, then the notes as
/// Live-Sessions-style rows (whole row switches, ✕ closes).
fn notes_schema(pane: &PaneState) -> Value {
    let store = &pane.store;
    let mut widgets: Vec<Value> = Vec::new();
    let mut toolbar = vec![
        json!({
            "action": "save", "label": "💾\u{fe0e}",
            "title": "Save the active note (Ctrl+S)",
            "primary": store.active().is_some_and(|note| note.dirty),
        }),
        json!({
            "action": "new_note", "label": "🗋\u{fe0e}",
            "title": "New note",
        }),
        json!({
            "action": "toggle_open_input", "label": "📂\u{fe0e}",
            "title": "Open or create a file by path",
        }),
    ];
    if store.active().is_some() {
        toolbar.push(json!({
            "action": "close_active", "label": "✕",
            "title": "Close the active note",
        }));
    }
    widgets.push(json!({ "kind": "toolbar", "id": "quick", "buttons": toolbar }));
    // The rename field: shown while a note's row menu asked to rename it,
    // prefilled with the current name. Enter applies; the ✕ menu / a switch
    // cancels. Named per-note in the id so switching which note is renamed
    // rebuilds the field with the new name (the value-epoch remount).
    if let Some(renaming) = &pane.renaming
        && let Some(note) = store.get(renaming)
    {
        widgets.push(json!({
            "kind": "text-input", "id": "rename_field",
            "placeholder": "new name…",
            "value": note.name(), "action": "rename_apply",
        }));
    }
    if pane.open_input {
        widgets.push(json!({
            "kind": "text-input", "id": "open_path",
            "placeholder": "open or create: ~/notes/todo.md",
            "value": "", "action": "open",
        }));
    }
    if let Some(conflict) = &pane.conflict {
        widgets.push(json!({
            "kind": "label", "muted": true,
            "text": format!("⚠ {conflict} changed on disk"),
        }));
        widgets.push(json!({
            "kind": "button", "id": "overwrite", "label": "Overwrite",
            "action": "overwrite", "primary": true,
        }));
        widgets.push(json!({
            "kind": "button", "id": "reload", "label": "Reload from disk",
            "action": "reload",
        }));
    }
    widgets.push(json!({
        "kind": "search-box", "id": "search",
        "placeholder": "Search notes (regex)…",
        "value": pane.search, "action": "search",
    }));
    // The tri-slider (Phase 4): yggterm renders `tabs` as its standard
    // segmented control, so the mode switch looks like every other yggui
    // mode switch.
    widgets.push(json!({
        "kind": "tabs", "id": "mode", "action": "set_mode",
        "active": store.view_mode.as_str(),
        "tabs": [
            { "id": "markdown", "label": "Markdown" },
            { "id": "split", "label": "Split" },
            { "id": "text", "label": "Text" },
        ],
    }));
    widgets.push(json!({
        "kind": "section", "text": "Files",
        "action": "new_note", "action_label": "+",
        "action_title": "New note",
    }));

    // The filter: regex when it compiles, substring otherwise; matched
    // against the note NAME and its full CONTENT (open notes carry their
    // buffers). A content hit reports its match count.
    let query = pane.search.trim();
    let matcher = regex::RegexBuilder::new(query)
        .case_insensitive(true)
        .build()
        .ok();
    let mut shown = 0usize;
    for note in &store.notes {
        let name = note.name();
        let (name_hit, content_hits) = if query.is_empty() {
            (true, 0)
        } else if let Some(re) = &matcher {
            (re.is_match(&name), re.find_iter(&note.content).count())
        } else {
            let needle = query.to_lowercase();
            (
                name.to_lowercase().contains(&needle),
                note.content.to_lowercase().matches(&needle).count(),
            )
        };
        if !name_hit && content_hits == 0 {
            continue;
        }
        shown += 1;
        let active = store.active_id.as_deref() == Some(note.id.as_str());
        let subtitle = if content_hits > 0 {
            format!("{content_hits} match{}", if content_hits == 1 { "" } else { "es" })
        } else {
            String::new()
        };
        let title = if note.dirty {
            format!("● {name}")
        } else {
            name
        };
        // `file:<ext>` — yggterm draws a rectangle badge carrying the
        // extension text ("md", "txt"; "·" when the file has none).
        let ext = note
            .path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        widgets.push(json!({
            "kind": "list-row",
            "id": note.id,
            "icon": format!("file:{ext}"),
            "title": title,
            "subtitle": subtitle,
            "selected": active,
            "row_action": "switch",
            "actions": [
                { "action": "close_note", "label": "✕", "title": "Close this note" },
            ],
            // Right-click menu: Rename (the only way to name an in-DB note) and
            // Close. yggterm draws it; the action carries this row's id back.
            "menu": [
                { "action": "rename", "label": "✎\u{fe0e}  Rename", "title": "Rename this note" },
                { "action": "close_note", "label": "✕  Close", "title": "Close this note" },
            ],
        }));
    }
    if shown == 0 {
        widgets.push(json!({
            "kind": "label", "muted": true,
            "text": if store.notes.is_empty() {
                "No files open. 🗋 creates one; 📂 opens a path.".to_string()
            } else {
                format!("No note matches \"{}\".", pane.search)
            },
        }));
    }
    // The rail STATUS FOOTER (yggterm pins it under the scroll area): wc of
    // the active note plus the wrap toggle. The counts reflect the store's
    // buffer — the live keystroke draft only reaches the daemon on actions
    // (draft sync/save), so mid-typing counts lag by one sync. Fine for a
    // status bar; a per-keystroke wc would put app logic GUI-side.
    let mut footer: Vec<Value> = Vec::new();
    if let Some(note) = store.active() {
        let words = note.content.split_whitespace().count();
        let lines = note.content.split('\n').count();
        let chars = note.content.chars().count();
        footer.push(json!({
            "kind": "label",
            "text": format!("{words} words \u{b7} {lines} lines \u{b7} {chars} chars"),
        }));
    }
    footer.push(json!({
        "kind": "toggle", "id": "wrap", "label": "Wrap",
        "action": "toggle_wrap", "value": store.word_wrap,
    }));
    json!({ "title": "Yedit", "widgets": widgets, "footer": footer })
}

/// Flush the editor draft up into the active note. Runs FIRST on every
/// action, so a mode toggle or tab switch never loses typed content. The
/// draft only exists while the plain editor is on screen.
fn absorb_editor_draft(pane: &mut PaneState, values: &Value) {
    let Some(draft) = values["editor"].as_str() else {
        return;
    };
    let Some(active_id) = pane.store.active_id.clone() else {
        return;
    };
    pane.store.edit(&active_id, draft.to_string());
}

fn handle_action(state: &Mutex<PaneState>, body: &Value) -> Value {
    let action = body["action"].as_str().unwrap_or_default();
    let values = &body["values"];
    // A widget's own value (a tab id, a row id, a toggle's next state) rides
    // `values.value` — yggterm's action POST shape (trap recorded 2026-07-17).
    let value = values["value"].as_str().unwrap_or_default().to_string();
    let mut pane = state.lock().unwrap();
    absorb_editor_draft(&mut pane, values);
    let mut toast: Option<String> = None;
    match action {
        // The GUI's debounced draft-sync (Phase 4): the absorb above already
        // did all the work — the draft is now in the store AND its sqlite
        // row, which is the crash-safety story. Nothing else to do.
        "draft" => {}
        "switch" => {
            if pane.store.notes.iter().any(|n| n.id == value) {
                pane.store.active_id = Some(value);
                pane.conflict = None;
                pane.renaming = None;
                pane.store.touch();
            }
        }
        // Row menu → Rename: open the rename field on the picked note (its id
        // rides `values.value`).
        "rename" => {
            if pane.store.notes.iter().any(|n| n.id == value) {
                pane.renaming = Some(value);
            }
        }
        "rename_apply" => {
            let new_name = values["rename_field"].as_str().unwrap_or_default().trim();
            if let Some(id) = pane.renaming.clone() {
                if new_name.is_empty() {
                    toast = Some("Enter a name".to_string());
                } else {
                    match pane.store.rename(&id, new_name) {
                        Ok(_) => {
                            pane.renaming = None;
                            pane.conflict = None;
                            toast = Some(format!("Renamed to {new_name}"));
                        }
                        Err(error) => toast = Some(format!("Rename failed: {error}")),
                    }
                }
            }
        }
        "rename_cancel" => {
            pane.renaming = None;
        }
        "set_mode" => {
            if let Some(mode) = crate::docs::ViewMode::parse(&value) {
                pane.store.view_mode = mode;
                pane.store.touch();
            }
        }
        "save" | "overwrite" => {
            let force = action == "overwrite";
            if let Some(id) = pane.store.active_id.clone() {
                let content = pane
                    .store
                    .get(&id)
                    .map(|note| note.content.clone())
                    .unwrap_or_default();
                match pane.store.save(&id, content, force) {
                    Ok(SaveOutcome::Saved { .. }) => {
                        pane.conflict = None;
                        toast = Some("Saved".to_string());
                    }
                    Ok(SaveOutcome::Conflict { .. }) => {
                        let name = pane
                            .store
                            .get(&id)
                            .map(|note| note.name())
                            .unwrap_or_default();
                        pane.conflict = Some(name);
                    }
                    Err(error) => toast = Some(format!("Save failed: {error}")),
                }
            }
        }
        "reload" => {
            if let Some(id) = pane.store.active_id.clone() {
                match pane.store.reload(&id) {
                    Ok(()) => {
                        pane.conflict = None;
                        toast = Some("Reloaded from disk".to_string());
                    }
                    Err(error) => toast = Some(format!("Reload failed: {error}")),
                }
            }
        }
        "close_active" => {
            if let Some(id) = pane.store.active_id.clone() {
                pane.store.close(&id);
                pane.conflict = None;
                pane.renaming = None;
            }
        }
        "close_note" => {
            if pane.store.notes.iter().any(|n| n.id == value) {
                pane.store.close(&value);
                pane.conflict = None;
                if pane.renaming.as_deref() == Some(value.as_str()) {
                    pane.renaming = None;
                }
            }
        }
        "search" => {
            pane.search = values["search"].as_str().unwrap_or_default().trim().to_string();
        }
        "new_note" => {
            let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let mut n = 1usize;
            let path = loop {
                let candidate = dir.join(format!("untitled-{n}.md"));
                let already_open = pane
                    .store
                    .notes
                    .iter()
                    .any(|note| note.path == candidate);
                if !candidate.exists() && !already_open {
                    break candidate;
                }
                n += 1;
            };
            match pane.store.open(&path) {
                Ok(_) => {
                    pane.renaming = None;
                    toast = Some(format!("New note {} (created on save)", path.display()));
                }
                Err(error) => toast = Some(format!("New note failed: {error}")),
            }
        }
        "toggle_open_input" => {
            pane.open_input = !pane.open_input;
        }
        "toggle_wrap" => {
            // The toggle's next state rides `values.value` ("true"/"false").
            pane.store.word_wrap = value == "true";
            // touch(): the editor widget's `word_wrap` lives in the DOCUMENT
            // schema, so the stamp must move for the viewport to refetch.
            pane.store.touch();
        }
        "open" => {
            let raw = values["open_path"].as_str().unwrap_or_default().trim().to_string();
            if raw.is_empty() {
                toast = Some("Enter a path to open".to_string());
            } else {
                match pane.store.open(Path::new(&raw)) {
                    Ok(_) => pane.open_input = false,
                    Err(error) => toast = Some(format!("Open failed: {error}")),
                }
            }
        }
        "open_recent" => {
            let recent: Option<PathBuf> = pane
                .store
                .recent
                .iter()
                .find(|p| format!("recent-{}", note_id(p)) == value)
                .cloned();
            if let Some(path) = recent {
                if let Err(error) = pane.store.open(&path) {
                    toast = Some(format!("Open failed: {error}"));
                }
            }
        }
        _ => toast = Some(format!("Unknown action {action}")),
    }
    // Answer with the schema of the pane that POSTED, and have the GUI
    // refetch the viewport when a sidebar action changed the document.
    let posting_pane = body["pane"].as_str().unwrap_or("doc");
    let schema = if posting_pane == "notes" {
        notes_schema(&pane)
    } else {
        document_schema(&pane)
    };
    let mut reply = json!({ "schema": schema });
    if posting_pane == "notes" && action != "search" {
        reply["refetch_document"] = Value::Bool(true);
    }
    if let Some(toast) = toast {
        reply["toast"] = Value::String(toast);
    }
    reply
}

/// The stamp under an already-held lock (route handlers).
fn version_of(pane: &PaneState) -> String {
    format!("{}:{}", pane.store.epoch, pane.conflict.is_some())
}

/// The declare stamp: schema content changes exactly when the store mutates.
pub fn document_version(state: &Mutex<PaneState>) -> String {
    let pane = state.lock().unwrap();
    format!("{}:{}", pane.store.epoch, pane.conflict.is_some())
}

fn respond_json(mut stream: TcpStream, status: u16, value: &Value) {
    let body = value.to_string();
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Error",
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len(),
    );
    let _ = stream.flush();
}
