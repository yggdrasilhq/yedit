//! yedit — a tabbed notepad / markdown reader, the second libyggterm consumer
//! and the DOCUMENT SURFACE pilot (2026-07-17).
//!
//! `yedit [file...]` inside a yggterm terminal declares ONE pane with
//! `placement: viewport`; yggterm renders its schema (note tabs, markdown
//! toggle, save, the rendered markdown or plain editor) as native shell DOM
//! in the main viewport. No web engine anywhere: yedit serves widgets and
//! markdown SOURCE from its control endpoint, yggterm paints. All state is
//! host-resident: buffers in this process, the session under
//! `~/.yggterm/yedit/`.

mod docs;
mod manifest;
mod osc;
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
    about = "Tabbed notepad / markdown reader for yggterm (libyggterm document-surface app)"
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
        eprintln!(
            "Control endpoint at {} — the document surface needs the yggterm GUI.",
            server.url
        );
    }

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))?;
    }

    // Foreground declare/heartbeat loop — a surface is a foreground program.
    // The declaration doubles as the liveness signal (the GUI expires the
    // contribution after ~15s of silence) and carries the document stamp, so
    // pane edits made through actions repaint within a heartbeat everywhere
    // else too (another GUI attached to the same daemon, for instance).
    if !session.is_empty() {
        osc::emit_declare(&session, &server.url, &server::document_version(&server.state));
        eprintln!("yedit: document surface declared — Ctrl+C to close.");
    }
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(4));
        if !session.is_empty() {
            osc::emit_declare(&session, &server.url, &server::document_version(&server.state));
        }
    }

    if !session.is_empty() {
        osc::emit_close(&session);
    }
    server.state.lock().unwrap().store.persist_session();
    Ok(())
}
