//! Tauri v5: v4 + distributable bundling.
//!
//! Backend discovery now supports three modes (in priority order):
//!   1. DSH_REPO env  → pnpm dsh inside that repo   (dev mode)
//!   2. walk up from cwd for apps/cli                (auto-discovered repo)
//!   3. dsh on PATH                                  (global install — installed .rpm/.deb/.AppImage path)

mod notify;

use anyhow::{anyhow, Context, Result};
use once_cell::sync::OnceCell;
use regex::Regex;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::Duration;
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(https?://[^\s/]+)/\?token=([A-Za-z0-9_\-]+)").unwrap());

static BACKEND_PGID: OnceCell<u32> = OnceCell::new();
static NOTIFY_SOCK: OnceCell<PathBuf> = OnceCell::new();
/// Whether a tray icon was created; without one, closing the window must exit.
static TRAY_READY: AtomicBool = AtomicBool::new(false);
/// Set while a controlled shutdown is in progress so the backend waiter stays quiet.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dsh_desktop_linux=info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_main_window(app);
                    }
                })
                .build(),
        )
        .setup(|app| {
            // GUI launches inherit a minimal PATH that often omits npm/nvm
            // locations. Merge the login-shell PATH before probing for `dsh`.
            ensure_login_path();

            match build_tray(app.handle()) {
                Ok(()) => {
                    TRAY_READY.store(true, Ordering::Relaxed);
                }
                Err(e) => {
                    tracing::error!(error = %e, "tray setup failed — window close will quit the app");
                }
            }

            // ── Path 1: X11/XWayland global shortcut (best-effort) ───────
            let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into());
            if session.eq_ignore_ascii_case("wayland") {
                tracing::warn!(
                    session = %session,
                    "Wayland session: XGrabKey-based shortcut may silently no-op. \
                     If Ctrl+Shift+D does nothing, bind ~/.local/bin/dsh-harness-toggle \
                     to a GNOME custom shortcut (SIGUSR1 path)."
                );
            }
            let toggle = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyD);
            match app.global_shortcut().register(toggle) {
                Ok(()) => tracing::info!(session = %session, "global shortcut Ctrl+Shift+D registered"),
                Err(e) => tracing::warn!(error = %e, "could not register Ctrl+Shift+D"),
            }

            // ── Path 2: SIGUSR1 → toggle window (Wayland-native) ─────────
            // ── SIGUSR2 → fire a test notification ───────────────────────
            #[cfg(unix)]
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sig = match signal(SignalKind::user_defined1()) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error = %e, "SIGUSR1 handler setup failed");
                            return;
                        }
                    };
                    tracing::info!("SIGUSR1 handler installed (kill -USR1 <pid> toggles window)");
                    loop {
                        sig.recv().await;
                        tracing::debug!("SIGUSR1 received, toggling window");
                        toggle_main_window(&handle);
                    }
                });

                tauri::async_runtime::spawn(async move {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sig = match signal(SignalKind::user_defined2()) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error = %e, "SIGUSR2 handler setup failed");
                            return;
                        }
                    };
                    tracing::info!("SIGUSR2 handler installed (kill -USR2 <pid> fires test notification)");
                    loop {
                        sig.recv().await;
                        notify::fire(
                            "DeepSeek Harness".into(),
                            "Test notification triggered via SIGUSR2".into(),
                            "normal".into(),
                        )
                        .await;
                    }
                });

                // ── SIGINT / SIGTERM / SIGHUP → clean shutdown ───────────
                // Without these, Ctrl+C or a session logout can leave the
                // backend process group running after the wrapper dies.
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sigint = signal(SignalKind::interrupt()).ok();
                    let mut sigterm = signal(SignalKind::terminate()).ok();
                    let mut sighup = signal(SignalKind::hangup()).ok();
                    let received = tokio::select! {
                        _ = async { match sigint.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending::<()>().await } } => "SIGINT",
                        _ = async { match sigterm.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending::<()>().await } } => "SIGTERM",
                        _ = async { match sighup.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending::<()>().await } } => "SIGHUP",
                    };
                    tracing::info!(signal = received, "terminating wrapper cleanly");
                    shutdown_and_exit(&handle);
                });
            }

            // ── Notify socket (bridge for backend / plugins) ─────────────
            match notify::prepare_socket() {
                Ok(path) => {
                    let _ = NOTIFY_SOCK.set(path.clone());
                    let sp = path.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = notify::listen(sp).await {
                            tracing::warn!(error = %e, "notify listener failed");
                        }
                    });
                }
                Err(e) => tracing::warn!(error = %e, "notify socket prepare failed"),
            }

            // ── Spawn backend, then open window ──────────────────────────
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match spawn_backend(handle.clone()).await {
                    Ok(url) => {
                        tracing::info!(%url, "backend ready, opening window");
                        if let Err(e) = open_main_window(&handle, &url) {
                            tracing::error!(error = %e, "failed to open window");
                            kill_backend();
                            handle.exit(1);
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "backend spawn failed");
                        notify::fire(
                            "DeepSeek Harness".into(),
                            format!("Backend failed to start: {e}"),
                            "critical".into(),
                        )
                        .await;
                        handle.exit(1);
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building Tauri app")
        .run(|_app, event| {
            if let RunEvent::Exit = event {
                SHUTTING_DOWN.store(true, Ordering::SeqCst);
                kill_backend();
                if let Some(sock) = NOTIFY_SOCK.get() {
                    notify::cleanup(sock);
                }
            }
        });
}

/// Merge the interactive login-shell PATH into this process so GUI launches can
/// find `dsh`, `node`, and pnpm even when the desktop session PATH is minimal.
fn ensure_login_path() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let login = std::process::Command::new(&shell)
        .args(["-lc", "printf %s \"$PATH\""])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|p| !p.is_empty());

    let Some(login) = login else { return };
    let current = std::env::var("PATH").unwrap_or_default();
    let mut parts: Vec<String> = Vec::new();
    for segment in login.split(':').chain(current.split(':')) {
        if segment.is_empty() || parts.iter().any(|p| p == segment) {
            continue;
        }
        parts.push(segment.to_string());
    }
    // SAFETY: called during single-threaded setup before worker threads probe the environment.
    std::env::set_var("PATH", parts.join(":"));
    tracing::debug!(path = %parts.join(":"), "merged login-shell PATH");
}

/// Resolve `dsh` through the login shell first, then through common install dirs.
fn find_dsh_on_path() -> Option<PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    if let Ok(out) = std::process::Command::new(&shell)
        .args(["-lc", "command -v dsh"])
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            let candidate = String::from_utf8_lossy(&out.stdout).trim().to_string();
            // `command -v` prints a bare name for shell functions/builtins;
            // only trust actual paths here.
            if candidate.starts_with('/') || candidate.contains('/') {
                return Some(PathBuf::from(candidate));
            }
        }
    }

    let mut dirs: Vec<PathBuf> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect();
    if let Ok(home) = std::env::var("HOME") {
        for rel in [
            ".local/bin",
            ".npm-global/bin",
            ".bun/bin",
            ".cargo/bin",
            ".local/share/pnpm",
        ] {
            dirs.push(PathBuf::from(&home).join(rel));
        }
    }
    for dir in [
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/home/linuxbrew/.linuxbrew/bin",
        "/opt/homebrew/bin",
    ] {
        dirs.push(PathBuf::from(dir));
    }

    dirs.into_iter()
        .map(|dir| dir.join("dsh"))
        .find(|candidate| candidate.is_file())
}

/// Idempotent controlled shutdown: stop the backend, remove the socket, exit.
fn shutdown_and_exit(app: &tauri::AppHandle) {
    if SHUTTING_DOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    kill_backend();
    if let Some(sock) = NOTIFY_SOCK.get() {
        notify::cleanup(sock);
    }
    app.exit(0);
}

fn toggle_main_window(app: &tauri::AppHandle) {
    let Some(w) = app.get_webview_window("main") else {
        tracing::debug!("toggle: no main window");
        return;
    };
    let visible = w.is_visible().unwrap_or(false);
    if visible {
        tracing::debug!("toggle: hiding");
        let _ = w.hide();
    } else {
        tracing::debug!("toggle: showing");
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn build_tray(app: &tauri::AppHandle) -> Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Window", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let icon = Image::from_bytes(include_bytes!("../icons/32x32.png"))?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .tooltip("DeepSeek Harness")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }
            "quit" => {
                shutdown_and_exit(app);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }
        })
        .build(app)?;

    Ok(())
}

fn kill_backend() {
    let Some(&pgid) = BACKEND_PGID.get() else {
        return;
    };
    tracing::info!(pgid, "killing backend process group");
    let _ = std::process::Command::new("sh")
        .args([
            "-c",
            &format!("kill -TERM -- -{} 2>/dev/null || true", pgid),
        ])
        .status();
    std::thread::sleep(Duration::from_millis(400));
    let _ = std::process::Command::new("sh")
        .args([
            "-c",
            &format!("kill -KILL -- -{} 2>/dev/null || true", pgid),
        ])
        .status();
}

fn open_main_window(app: &tauri::AppHandle, url: &str) -> Result<()> {
    let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
        .title("DeepSeek Harness")
        .inner_size(1280.0, 800.0)
        .min_inner_size(900.0, 600.0)
        .visible(true)
        .build()
        .context("build webview window")?;

    let win_for_close = win.clone();
    win.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            // Closing to the tray only makes sense when a tray exists; otherwise
            // the app would become unreachable, so let the window close.
            if TRAY_READY.load(Ordering::Relaxed) {
                api.prevent_close();
                if let Err(e) = win_for_close.hide() {
                    tracing::warn!(error = %e, "hide-on-close failed");
                }
            }
        }
    });

    Ok(())
}

// ── backend discovery + spawn ───────────────────────────────────────────────

struct DshBackend {
    bin: String,
    prefix_args: Vec<String>,
    workdir: PathBuf,
    note: &'static str,
}

/// Locate a `dsh` backend. Priority:
///   1. DSH_REPO env  → `pnpm dsh` inside that repo   (dev mode)
///   2. walk up from cwd for `apps/cli`                (auto-discovered repo)
///   3. `dsh` on PATH                                  (global install — distribution)
fn locate_dsh_backend() -> Result<DshBackend> {
    // 1. DSH_REPO env
    if let Ok(repo) = std::env::var("DSH_REPO") {
        let p = std::path::Path::new(&repo);
        if !p.join("apps/cli").exists() {
            return Err(anyhow!("DSH_REPO='{}' has no apps/cli subdir", repo));
        }
        return Ok(DshBackend {
            bin: "pnpm".into(),
            prefix_args: vec!["dsh".into()],
            workdir: p.to_path_buf(),
            note: "DSH_REPO dev mode",
        });
    }

    // 2. Auto-discover repo from cwd
    if let Ok(cwd) = std::env::current_dir() {
        for ancestor in cwd.ancestors() {
            if ancestor.join("apps/cli").exists() {
                return Ok(DshBackend {
                    bin: "pnpm".into(),
                    prefix_args: vec!["dsh".into()],
                    workdir: ancestor.to_path_buf(),
                    note: "auto-discovered repo",
                });
            }
        }
    }

    // 3. `dsh` on PATH (global install)
    if let Some(bin) = find_dsh_on_path() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        return Ok(DshBackend {
            bin: bin.to_string_lossy().into_owned(),
            prefix_args: vec![],
            workdir: cwd,
            note: "dsh on PATH (global install)",
        });
    }

    Err(anyhow!(
        "no dsh backend found. Install one of:\n\
         • Global:  npm install -g @deepseek-ai/dsh\n\
         • Dev:     set DSH_REPO=/path/to/deepseek-harness before launching\n\
         • Or launch from inside a repo checkout that has apps/cli"
    ))
}

async fn spawn_backend(app: tauri::AppHandle) -> Result<String> {
    let backend = locate_dsh_backend()?;
    tracing::info!(
        bin = %backend.bin,
        args = ?backend.prefix_args,
        cwd = %backend.workdir.display(),
        note = %backend.note,
        "spawning dsh web"
    );

    let mut cmd = Command::new(&backend.bin);
    cmd.args(&backend.prefix_args)
        .args(["web", "--port", "0", "--no-open"])
        .current_dir(&backend.workdir)
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .process_group(0);

    if let Some(sock) = NOTIFY_SOCK.get() {
        cmd.env("DSH_NOTIFY_SOCK", sock.to_string_lossy().into_owned());
    }

    let mut child = cmd.spawn().context("spawn dsh web")?;

    let pid = child.id().ok_or_else(|| anyhow!("child pid missing"))?;
    let _ = BACKEND_PGID.set(pid);
    tracing::info!(pgid = pid, "backend spawned");

    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let tx = std::sync::Arc::new(tokio::sync::Mutex::new(Some(tx)));

    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "dsh.stdout", "{}", line);
            if let Some(c) = URL_RE.captures(&line) {
                let url = format!("{}/?token={}", &c[1], &c[2]);
                if let Some(s) = tx.lock().await.take() {
                    let _ = s.send(url);
                }
                break;
            }
        }
    });

    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!(target: "dsh.stderr", "{}", line);
        }
    });

    let startup_secs = std::env::var("DSH_STARTUP_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(120);

    let url = match tokio::time::timeout(Duration::from_secs(startup_secs), rx).await {
        Ok(Ok(u)) => u,
        Ok(Err(_)) => return Err(anyhow!("backend stdout closed before URL")),
        Err(_) => return Err(anyhow!("backend did not print URL in {startup_secs}s")),
    };

    let authority = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .ok_or_else(|| anyhow!("malformed URL: {url}"))?
        .to_owned();

    for _ in 0..40 {
        if tokio::net::TcpStream::connect(&authority).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    tokio::spawn(async move {
        let exit = child.wait().await;
        tracing::info!(?exit, "backend exited");
        if SHUTTING_DOWN.load(Ordering::SeqCst) {
            return;
        }
        let failed = !matches!(&exit, Ok(status) if status.success());
        if failed {
            notify::fire(
                "DeepSeek Harness".into(),
                format!("Backend exited unexpectedly ({exit:?})"),
                "critical".into(),
            )
            .await;
        }
        // The window now points at a dead server; close the wrapper instead of
        // leaving a blank, unusable window behind.
        SHUTTING_DOWN.store(true, Ordering::SeqCst);
        app.exit(if failed { 1 } else { 0 });
    });

    Ok(url)
}
