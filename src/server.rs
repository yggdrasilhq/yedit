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
        // `value_key` is the note's id: the editor is ONE slot that holds many
        // buffers over its life, and yggterm needs to know which one is loaded
        // to decide whether the field reloads (two new files are both empty, so
        // the text cannot tell them apart) and to name the draft's owner when
        // it posts it back.
        Some(note) if store.view_mode == ViewMode::Split => {
            widgets.push(json!({
                "kind": "text-input", "id": "editor", "multiline": true,
                "line_numbers": true, "word_wrap": store.word_wrap,
                "value": note.content, "value_key": note.id,
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
                "value": note.content, "value_key": note.id,
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
    // NOTE: renaming is declared ON THE ROW (`rename` below), not as a field
    // floating above the list. A separate input elsewhere in the rail made the
    // user hunt for where their typing went; yggterm's row rename replaces the
    // row body in place — the same shape a Live Sessions rename has — and
    // carries the ✨ generate button for free.
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
        // The save state is a STATUS, not part of the name. It used to be a
        // literal `●` glued onto the title, which painted in the row's text
        // colour (a black dot in the light theme) and shoved every name one
        // character right. yggterm's `list-row` now has the status slot the
        // native session rows always had; the app only says which class.
        //
        // The vocabulary is yggterm's durability vocabulary (DESIGN.md
        // "Status indicator vocabulary"), and it lines up exactly:
        //   transient (BLUE)  = the content lives ONLY in yedit's sqlite draft
        //                       row — dirty, unsaved, nothing on disk yet
        //   durable   (GREEN) = the content is the file on disk
        //   ""                = a brand-new note never typed into: no draft
        //                       row, no file. Empty slot, not a third colour.
        let status = if note.dirty {
            "transient"
        } else if note.path.exists() {
            "durable"
        } else {
            ""
        };
        // `file:<ext>` — yggterm draws a rectangle badge carrying the
        // extension text ("md", "txt"; "·" when the file has none).
        let ext = note
            .path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let mut row = json!({
            "kind": "list-row",
            "id": note.id,
            "icon": format!("file:{ext}"),
            "title": name,
            "status": status,
            "subtitle": subtitle,
            "selected": active,
            "row_action": "switch",
            // Rows are draggable to reorder. The order IS `store.notes`, so a
            // drop is just a permutation of that vec — and it persists with the
            // session, because a list the user arranged and lost on restart is
            // worse than one that never moved.
            "reorder_action": "reorder_notes",
            "actions": [
                { "action": "close_note", "label": "✕", "title": "Close this note" },
            ],
            // Right-click menu: Rename (the only way to name an in-DB note) and
            // Close. yggterm draws it; the action carries this row's id back.
            "menu": [
                { "action": "rename", "label": "✎\u{fe0e}  Rename", "title": "Rename this note" },
                { "action": "close_note", "label": "✕  Close", "title": "Close this note" },
            ],
        });
        // The row being renamed becomes an in-place field. `ai_source` is the
        // note's own text, which is what makes yggterm show the ✨ button and
        // name the note from its content — yggterm owns the LLM settings, this
        // app only says WHAT to name.
        if pane.renaming.as_deref() == Some(note.id.as_str()) {
            row["rename"] = json!({
                "value": note.name(),
                "action": "rename_apply",
                "cancel_action": "rename_cancel",
                "ai_source": note.content,
                "placeholder": "new name…",
            });
        }
        widgets.push(row);
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

/// Which note an incoming editor draft belongs to.
///
/// yggterm names the buffer under `value_keys.editor` — the `value_key` this
/// app declared on the editor widget when it handed that text out. That is the
/// ONLY trustworthy target: the debounced draft sync lands seconds after the
/// keystrokes, and by then `active_id` may already be a different note. Writing
/// a late draft into whatever happens to be active is how one file's text got
/// written into another's.
///
/// `None` from an older yggterm that declares no identity ⇒ fall back to the
/// active note, which is exactly the pre-identity behaviour. Refusing instead
/// would mean the user could not type at all against an old GUI, and losing
/// every keystroke is worse than the race this replaces.
fn draft_target_id(pane: &PaneState, value_keys: &Value) -> Option<String> {
    let target = match value_keys["editor"].as_str() {
        Some(id) => id.to_string(),
        None => pane.store.active_id.clone()?,
    };
    // A note closed while its draft was in flight takes the draft with it.
    pane.store
        .notes
        .iter()
        .any(|note| note.id == target)
        .then_some(target)
}

/// Flush the editor draft up into the note it BELONGS to. Runs FIRST on every
/// action, so a mode toggle or tab switch never loses typed content. The
/// draft only exists while the plain editor is on screen.
fn absorb_editor_draft(pane: &mut PaneState, values: &Value, value_keys: &Value) {
    let Some(draft) = values["editor"].as_str() else {
        return;
    };
    let Some(target) = draft_target_id(pane, value_keys) else {
        return;
    };
    pane.store.edit(&target, draft.to_string());
}

fn handle_action(state: &Mutex<PaneState>, body: &Value) -> Value {
    let action = body["action"].as_str().unwrap_or_default();
    let values = &body["values"];
    // Sibling of `values`, not a member of it: `values` is a flat
    // {widget id: draft} map. `value_keys` says WHICH buffer each of those
    // drafts came out of.
    let value_keys = &body["value_keys"];
    // A widget's own value (a tab id, a row id, a toggle's next state) rides
    // `values.value` — yggterm's action POST shape (trap recorded 2026-07-17).
    let value = values["value"].as_str().unwrap_or_default().to_string();
    let mut pane = state.lock().unwrap();
    absorb_editor_draft(&mut pane, values, value_keys);
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
            // The row's inline field posts under `rename:<row id>` (yggterm's
            // widget-id rule for a row rename). The old `rename_field` key was
            // the floating input that no longer exists.
            let draft_key = format!("rename:{}", pane.renaming.clone().unwrap_or_default());
            let new_name = values[&draft_key].as_str().unwrap_or_default().trim();
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
        // A row was dragged to a new slot. yggterm sends the pane's whole new
        // order in `values.order`; adopt it wholesale rather than re-deriving
        // the move — the GUI already resolved before-vs-after against the rows
        // the user was actually looking at.
        "reorder_notes" => {
            let order: Vec<String> = values["order"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if pane.store.reorder_notes(&order) {
                pane.store.touch();
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "yedit-server-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A pane holding notes. A name in `on_disk` opens SAVED; a name in `new`
    /// opens as a brand-new never-saved note — the state the collision lives in.
    fn pane_with(tag: &str, on_disk: &[&str], new: &[&str]) -> (PathBuf, Mutex<PaneState>) {
        let home = temp_home(tag);
        let mut store = Store::new(home.clone());
        for name in on_disk {
            let file = home.join(name);
            std::fs::write(&file, format!("body of {name}")).unwrap();
            store.open(&file).unwrap();
        }
        for name in new {
            store.open(&home.join(name)).unwrap();
        }
        (
            home,
            Mutex::new(PaneState {
                store,
                conflict: None,
                search: String::new(),
                open_input: false,
                renaming: None,
            }),
        )
    }

    fn note_ids(state: &Mutex<PaneState>) -> Vec<String> {
        state
            .lock()
            .unwrap()
            .store
            .notes
            .iter()
            .map(|note| note.id.clone())
            .collect()
    }

    fn content_of(state: &Mutex<PaneState>, id: &str) -> String {
        state
            .lock()
            .unwrap()
            .store
            .get(id)
            .expect("the note is open")
            .content
            .clone()
    }

    // THE corruption (YS-1). The GUI's draft sync is debounced: it POSTs the
    // editor buffer seconds after the keystrokes. Paste into note A, click note
    // B before it fires, and the draft lands while B is active. Applying it to
    // "whatever is active now" wrote A's text into B — silently, over the
    // user's file. The POST names its buffer; honour that name.
    #[test]
    fn a_late_draft_lands_in_the_note_it_was_typed_in_not_the_active_one() {
        let (home, state) = pane_with("late-draft", &[], &["untitled-1.md", "untitled-2.md"]);
        let ids = note_ids(&state);
        let (a, b) = (ids[0].clone(), ids[1].clone());

        // The user is now on note B (they clicked it).
        state.lock().unwrap().store.active_id = Some(b.clone());

        // A's draft arrives late, naming A.
        handle_action(
            &state,
            &json!({
                "pane": "doc",
                "action": "draft",
                "values": { "editor": "pasted into A" },
                "value_keys": { "editor": a },
            }),
        );

        assert_eq!(content_of(&state, &a), "pasted into A", "A keeps its text");
        assert_eq!(content_of(&state, &b), "", "B must not inherit A's paste");
        assert_eq!(
            state.lock().unwrap().store.active_id.as_deref(),
            Some(b.as_str()),
            "absorbing a draft never moves the user"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    // A draft for a note that was closed while it was in flight is dropped, not
    // redirected onto a neighbour.
    #[test]
    fn a_draft_for_a_note_that_is_gone_is_dropped() {
        let (home, state) = pane_with("gone-draft", &[], &["only.md"]);
        let id = note_ids(&state)[0].clone();
        handle_action(
            &state,
            &json!({
                "pane": "doc",
                "action": "draft",
                "values": { "editor": "text for a closed note" },
                "value_keys": { "editor": "a-note-id-that-is-not-open" },
            }),
        );
        assert_eq!(content_of(&state, &id), "", "the open note is untouched");
        let _ = std::fs::remove_dir_all(&home);
    }

    // Back-compat: an older yggterm sends no `value_keys` at all. Falling back
    // to the active note is exactly the pre-identity behaviour — REFUSING here
    // would mean the user could not type at all against an old GUI, and losing
    // every keystroke is worse than the race it replaces.
    #[test]
    fn a_draft_with_no_declared_target_still_reaches_the_active_note() {
        let (home, state) = pane_with("no-keys", &[], &["only.md"]);
        let id = note_ids(&state)[0].clone();
        handle_action(
            &state,
            &json!({ "pane": "doc", "action": "draft", "values": { "editor": "typed" } }),
        );
        assert_eq!(content_of(&state, &id), "typed");
        let _ = std::fs::remove_dir_all(&home);
    }

    // The editor declares WHICH note it is holding, so yggterm can tell two
    // empty new files apart and remount the textarea between them.
    //
    // EVERY view mode that renders an editor, not just the default: the modes
    // are separate match arms, and an arm that forgets the identity is exactly
    // the state the bug lived in.
    #[test]
    fn every_editor_widget_declares_the_note_it_is_holding() {
        use crate::docs::ViewMode;
        let (home, state) = pane_with("value-key", &["a.md"], &[]);
        let id = note_ids(&state)[0].clone();
        let mut seen = 0usize;
        for mode in [ViewMode::Split, ViewMode::Text, ViewMode::Markdown] {
            state.lock().unwrap().store.view_mode = mode;
            let pane = state.lock().unwrap();
            let schema = document_schema(&pane);
            for widget in schema["widgets"].as_array().unwrap() {
                if widget["kind"] != "text-input" {
                    continue;
                }
                seen += 1;
                assert_eq!(
                    widget["value_key"],
                    json!(id),
                    "the {mode:?} editor must name the note it is holding"
                );
            }
        }
        assert_eq!(
            seen, 2,
            "Split and Text each render an editor; Markdown does not"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    // YS-4. The save state is a STATUS, not part of the name: a `●` glued onto
    // the title painted in the row's text colour and shifted every name one
    // character right. BLUE (transient) = the content lives only in yedit's
    // sqlite drafts row; GREEN (durable) = it is the file on disk.
    #[test]
    fn a_rows_save_state_is_a_status_class_not_a_glyph_in_its_title() {
        let (home, state) = pane_with("status", &["saved.md"], &["brand-new.md"]);
        let ids = note_ids(&state);
        let (saved, fresh) = (ids[0].clone(), ids[1].clone());

        // Type into the saved note so it goes dirty.
        state
            .lock()
            .unwrap()
            .store
            .edit(&saved, "edited".to_string());

        let pane = state.lock().unwrap();
        let schema = notes_schema(&pane);
        let row = |id: &str| {
            schema["widgets"]
                .as_array()
                .unwrap()
                .iter()
                .find(|w| w["id"] == id)
                .unwrap_or_else(|| panic!("row {id} is in the rail"))
                .clone()
        };

        assert_eq!(
            row(&saved)["status"],
            json!("transient"),
            "dirty lives in the db"
        );
        assert_eq!(
            row(&saved)["title"],
            json!("saved.md"),
            "the name is just the name"
        );
        assert_eq!(
            row(&fresh)["status"],
            json!(""),
            "a new note never typed into is neither in the db nor on disk — empty slot"
        );
        for id in [&saved, &fresh] {
            let title = row(id)["title"].as_str().unwrap().to_string();
            assert!(
                !title.contains('\u{25cf}'),
                "status must not live in the title: {title:?}"
            );
        }
        drop(pane);

        // Saving flips it to the durable class.
        let content = state
            .lock()
            .unwrap()
            .store
            .get(&saved)
            .unwrap()
            .content
            .clone();
        state
            .lock()
            .unwrap()
            .store
            .save(&saved, content, false)
            .unwrap();
        let pane = state.lock().unwrap();
        let schema = notes_schema(&pane);
        let row = schema["widgets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["id"] == saved)
            .unwrap();
        assert_eq!(row["status"], json!("durable"), "saved is on disk");
        drop(pane);
        let _ = std::fs::remove_dir_all(&home);
    }
}
