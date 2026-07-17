//! The libyggterm OSC 7717 channel — yedit's side of the surface contract.
//!
//! yedit is a DOCUMENT-SURFACE app (2026-07-17): ONE pane, placement
//! "viewport", rendered by yggterm as native shell DOM in the main viewport.
//! No web surface, no child webview. The declaration carries a
//! `document_version` stamp; the GUI refetches the pane schema only when it
//! moves. Contract: yggterm `.agents/skills/libyggterm-surfaces/SKILL.md`.

use base64::Engine as _;
use serde_json::json;
use std::io::Write as _;

fn emit(verb: &str, action: &str, payload: &str) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut stdout = std::io::stdout().lock();
    let _ = write!(stdout, "\u{1b}]7717;{verb};{action};{encoded}\u{7}");
    let _ = stdout.flush();
}

/// `sidebar ; declare` — idempotent, re-emitted every ~4s as the liveness
/// signal (an unswept contribution expires like a surface). `document_version`
/// is the change stamp over the pane's content.
pub fn emit_declare(session: &str, control: &str, document_version: &str) {
    let payload = json!({
        "session": session,
        "control": control,
        "app_name": "Yedit",
        "document_version": document_version,
        "panes": [
            {
                // U+FE0E forces text presentation so the glyph sits with
                // yggterm's monochrome chrome instead of a colour emoji.
                "id": "doc",
                "icon": "🗒\u{fe0e}",
                "title": "Yedit (tabbed notepad / markdown reader)",
                "placement": "viewport",
            },
        ],
    });
    emit("sidebar", "declare", &payload.to_string());
}

pub fn emit_close(session: &str) {
    emit(
        "sidebar",
        "close",
        &json!({ "session": session }).to_string(),
    );
}
