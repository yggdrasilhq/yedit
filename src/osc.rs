//! The libyggterm OSC 7717 channel — yedit's side of the surface contract.
//!
//! The app writes OSC escape sequences to its own stdout; the PTY relay
//! carries them to the yggterm GUI. Unknown OSCs are invisible in a plain
//! terminal — that is the degradation story. Contract:
//! yggterm `.agents/skills/libyggterm-surfaces/SKILL.md`.

use base64::Engine as _;
use serde_json::json;
use std::io::Write as _;

fn emit(verb: &str, action: &str, payload: &str) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\u{1b}]7717;{verb};{action};{encoded}\u{7}");
    let _ = stdout.flush();
}

/// `web-surface ; open|heartbeat|close`. The GUI expires a surface after ~15s
/// without a heartbeat, so a SIGKILLed yedit never leaks an overlay.
/// Heartbeats can never create or navigate a surface — only `open` can.
pub fn emit_web_surface(action: &str, session: &str, url: &str, title: &str) {
    let payload = json!({
        "session": session,
        "url": url,
        "title": title,
        "start_page": false,
    });
    emit("web-surface", action, &payload.to_string());
}

/// `sidebar ; declare` — idempotent, re-emitted on the heartbeat cadence as
/// the liveness signal. Carries only the control endpoint and pane buttons;
/// the GUI fetches the schema from `<control>/pane/notes` when opened.
/// Emit BEFORE the first `web-surface ; open` (DECLARE BEFORE OPEN).
pub fn emit_sidebar_declare(session: &str, control: &str) {
    let payload = json!({
        "session": session,
        "control": control,
        "app_name": "Yedit",
        "panes": [
            {
                // U+FE0E forces text presentation so the glyph sits with
                // yggterm's monochrome chrome instead of a colour emoji.
                "id": "notes",
                "icon": "🗒\u{fe0e}",
                "title": "Yedit notes (open tabs, recent files)",
            },
        ],
    });
    emit("sidebar", "declare", &payload.to_string());
}

pub fn emit_sidebar_close(session: &str) {
    emit(
        "sidebar",
        "close",
        &json!({ "session": session }).to_string(),
    );
}
