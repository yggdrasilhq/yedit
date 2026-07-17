//! yedit — a tabbed notepad / markdown reader, the second libyggterm consumer.
//!
//! `yedit [file...]` inside a yggterm terminal takes over the viewport with a
//! rendered-markdown / plain-editor page and contributes a Notes pane
//! (vertical note tabs + open/recent picker) to the right rail. All state is
//! host-resident: buffers in this process, the session under
//! `~/.yggterm/yedit/`. yggterm renders; yedit owns.
//!
//! Outside yggterm (no `YGGTERM_SESSION_ID`) v1 prints the server URL for a
//! regular browser instead of opening a standalone window — the GUI-window
//! fallback is deliberately deferred (ychrome owns that pattern; extract when
//! needed).

mod docs;
mod manifest;
mod osc;
mod render;
mod server;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "yedit",
    version,
    about = "Tabbed notepad / markdown reader for yggterm (libyggterm app)"
)]
struct Args {
    /// Files to open as tabs. None ⇒ restore the previous session's tabs.
    files: Vec<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    manifest::write_best_effort();

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut store = docs::Store::new(home);
    for file in &args.files {
        store.open(file)?;
    }

    let server = server::spawn(store)?;
    let session = std::env::var("YGGTERM_SESSION_ID").unwrap_or_default();

    if session.is_empty() {
        eprintln!("yedit: not inside yggterm (YGGTERM_SESSION_ID unset).");
        eprintln!("Serving at {} — open it in a browser. Ctrl+C quits.", server.url);
    }

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))?;
    }

    let title = |state: &std::sync::Mutex<docs::Store>| -> String {
        let store = state.lock().unwrap();
        match store.active() {
            Some(note) => format!("{} — yedit", note.name()),
            None => "yedit".to_string(),
        }
    };

    if !session.is_empty() {
        // DECLARE BEFORE OPEN: the sidebar contribution must be live before
        // the first surface open (libyggterm-surfaces contract).
        osc::emit_sidebar_declare(&session, &server.url);
        osc::emit_web_surface("open", &session, &server.url, &title(&server.state));
        eprintln!("yedit: surface open at {} — Ctrl+C to close.", server.url);
    }

    // Foreground heartbeat loop — a surface is a foreground program. The ~4s
    // cadence keeps surface + contribution alive (the GUI expires both after
    // ~15s of silence, so a SIGKILLed yedit never leaks an overlay).
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(4));
        if session.is_empty() {
            continue;
        }
        osc::emit_sidebar_declare(&session, &server.url);
        osc::emit_web_surface("heartbeat", &session, &server.url, &title(&server.state));
    }

    if !session.is_empty() {
        osc::emit_web_surface("close", &session, &server.url, "yedit");
        osc::emit_sidebar_close(&session);
    }
    server.state.lock().unwrap().persist_session();
    Ok(())
}
