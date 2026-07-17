//! yedit — a tabbed notepad / markdown reader, the second libyggterm consumer
//! and the DOCUMENT SURFACE pilot (2026-07-17).
//!
//! **Two-tier shape (libyggterm Phase 4, the emacsclient model): the daemon
//! is durable; the client is disposable.** A per-host `yedit --daemon` owns
//! the note store, the sqlite drafts db, and the control endpoint. `yedit
//! [file...]` is a thin client: it ensures the daemon is running, routes the
//! files into it over `POST /open`, emits the OSC declare for THIS terminal
//! session, and exits — the shell comes back, and the yggterm GUI keeps the
//! surface alive by pinging `<control>/ping` (libyggterm Phase 2). No web
//! engine anywhere: the daemon serves widgets and markdown SOURCE; yggterm
//! paints them as shell DOM.

mod docs;
mod manifest;
mod osc;
mod server;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "yedit",
    version,
    about = "Tabbed notepad / markdown reader for yggterm (libyggterm document-surface app)"
)]
struct Args {
    /// Files to open as tabs. None ⇒ show the daemon's current session.
    files: Vec<PathBuf>,
    /// Run the durable per-host daemon (normally auto-spawned by the client).
    #[arg(long, hide = true)]
    daemon: bool,
    /// Close this terminal session's yedit surface (the daemon keeps running).
    #[arg(long)]
    close: bool,
}

fn state_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    docs::Store::state_dir(&home)
}

fn control_url_path() -> PathBuf {
    state_dir().join("control-url")
}

/// GET `<control>/ping` with a short fuse; `Some(document_version)` iff the
/// daemon answered. Answering IS the liveness test — a stale url file or a
/// dead daemon both read as "not running".
fn ping(control_url: &str) -> Option<String> {
    use std::io::{Read as _, Write as _};
    let address = control_url.strip_prefix("http://")?;
    let (host_port, _) = address.split_once('/').unwrap_or((address, ""));
    let mut stream = std::net::TcpStream::connect_timeout(
        &host_port.parse().ok()?,
        Duration::from_millis(800),
    )
    .ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
    stream
        .write_all(
            format!("GET /ping HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let body = raw.split("\r\n\r\n").nth(1)?;
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    value
        .get("document_version")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// POST a JSON body to the daemon; the reply body as JSON.
fn post(control_url: &str, path: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
    use std::io::{Read as _, Write as _};
    let address = control_url
        .strip_prefix("http://")
        .context("control url is not http")?;
    let (host_port, _) = address.split_once('/').unwrap_or((address, ""));
    let mut stream = std::net::TcpStream::connect_timeout(
        &host_port.parse().context("control url host:port")?,
        Duration::from_millis(1500),
    )
    .context("connecting to the yedit daemon")?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(3000)));
    let payload = body.to_string();
    stream.write_all(
        format!(
            "POST {path} HTTP/1.1\r\nHost: {host_port}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        )
        .as_bytes(),
    )?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    let body = raw
        .split("\r\n\r\n")
        .nth(1)
        .context("daemon reply has no body")?;
    serde_json::from_str(body.trim()).context("daemon reply is not json")
}

/// The running daemon's control url — starting one if none answers. The url
/// file is only trusted when its endpoint PINGS; a stale file (machine
/// reboot, killed daemon) is overwritten by the fresh spawn.
fn ensure_daemon() -> Result<String> {
    if let Ok(url) = std::fs::read_to_string(control_url_path()) {
        let url = url.trim().to_string();
        if !url.is_empty() && ping(&url).is_some() {
            return Ok(url);
        }
    }
    let exe = std::env::current_exe().context("locating the yedit binary")?;
    let mut command = std::process::Command::new(exe);
    command
        .arg("--daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        // The daemon must not die with this client's terminal: new session,
        // no controlling TTY, cwd pinned to home (a daemon's cwd must never
        // hold a mount or a soon-deleted directory hostage).
        .current_dir(dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    command.spawn().context("spawning the yedit daemon")?;
    // Wait for the fresh daemon to bind and write its url.
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(url) = std::fs::read_to_string(control_url_path()) {
            let url = url.trim().to_string();
            if !url.is_empty()
                && let Some(_version) = ping(&url)
            {
                return Ok(url);
            }
        }
    }
    anyhow::bail!("the yedit daemon did not come up within 5s")
}

/// The durable half: store + drafts db + control endpoint, forever. SIGTERM/
/// SIGINT persist the session and exit; drafts are already durable (every
/// edit writes its row through).
fn run_daemon() -> Result<()> {
    manifest::write_best_effort();
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let store = docs::Store::new(home);
    let server = server::spawn(store)?;
    std::fs::create_dir_all(state_dir())?;
    std::fs::write(control_url_path(), &server.url)?;

    let state = server.state.clone();
    ctrlc::set_handler(move || {
        state.lock().unwrap().store.persist_session();
        std::process::exit(0);
    })?;
    eprintln!("yedit daemon: control endpoint at {}", server.url);
    loop {
        std::thread::park();
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.daemon {
        return run_daemon();
    }

    let session = std::env::var("YGGTERM_SESSION_ID").unwrap_or_default();
    if args.close {
        if session.is_empty() {
            anyhow::bail!("yedit --close needs a yggterm session (YGGTERM_SESSION_ID unset)");
        }
        osc::emit_close(&session);
        println!("yedit: surface closed (the daemon keeps running).");
        return Ok(());
    }

    let control_url = ensure_daemon()?;
    // Resolve HERE, against the CLIENT's cwd — the daemon's cwd is its own.
    for file in &args.files {
        let absolute = if file.is_absolute() {
            file.clone()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(file))
                .unwrap_or_else(|_| file.clone())
        };
        let reply = post(
            &control_url,
            "/open",
            &serde_json::json!({ "path": absolute.to_string_lossy() }),
        )?;
        if reply["ok"].as_bool() != Some(true) {
            eprintln!(
                "yedit: open {} failed: {}",
                absolute.display(),
                reply["error"].as_str().unwrap_or("unknown error")
            );
        }
    }

    let version = ping(&control_url).context("the yedit daemon stopped answering")?;
    if session.is_empty() {
        eprintln!("yedit: not inside yggterm (YGGTERM_SESSION_ID unset).");
        eprintln!("Daemon control endpoint at {control_url} — the document surface needs the yggterm GUI.");
        return Ok(());
    }
    // Declare and EXIT — the shell comes back. The GUI's endpoint pings
    // (libyggterm Phase 2) keep the surface alive from here; `yedit --close`
    // or the surface ✕ retire it.
    osc::emit_declare(&session, &control_url, &version);
    println!("yedit: document surface opened — `yedit --close` to close it.");
    Ok(())
}
