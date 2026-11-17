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

static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"http://127\.0\.0\.1:(\d+)/\?token=([A-Za-z0-9_\-]+)").unwrap()
});

static BACKEND_PGID: OnceCell<u32> = OnceCell::new();
static NOTIFY_SOCK: OnceCell<PathBuf> = OnceCell::new();

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
            if let Err(e) = build_tray(app.handle()) {
                tracing::error!(error = %e, "tray setup failed — continuing without tray");
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
                match spawn_backend().await {
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
                kill_backend();
                if let Some(sock) = NOTIFY_SOCK.get() {
                    notify::cleanup(sock);
                }
            }
        });
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
                kill_backend();
                app.exit(0);
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
    let Some(&pgid) = BACKEND_PGID.get() else { return };
    tracing::info!(pgid, "killing backend process group");
    let _ = std::process::Command::new("sh")
        .args(["-c", &format!("kill -TERM -- -{} 2>/dev/null || true", pgid)])
        .status();
    std::thread::sleep(Duration::from_millis(400));
    let _ = std::process::Command::new("sh")
        .args(["-c", &format!("kill -KILL -- -{} 2>/dev/null || true", pgid)])
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
            api.prevent_close();
            if let Err(e) = win_for_close.hide() {
                tracing::warn!(error = %e, "hide-on-close failed");
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
    if let Ok(out) = std::process::Command::new("sh")
        .arg("-c")
        .arg("command -v dsh")
        .output()
    {
        if out.status.success() {
            let bin = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !bin.is_empty() {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                return Ok(DshBackend {
                    bin,
                    prefix_args: vec![],
                    workdir: cwd,
                    note: "dsh on PATH (global install)",
                });
            }
        }
    }

    Err(anyhow!(
        "no dsh backend found. Install one of:\n\
         • Global:  npm install -g @deepseek-ai/dsh\n\
         • Dev:     set DSH_REPO=/path/to/deepseek-harness before launching\n\
         • Or launch from inside a repo checkout that has apps/cli"
    ))
}

async fn spawn_backend() -> Result<String> {
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
                let url = format!("http://127.0.0.1:{}/?token={}", &c[1], &c[2]);
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

    let url = match tokio::time::timeout(Duration::from_secs(60), rx).await {
        Ok(Ok(u)) => u,
        Ok(Err(_)) => return Err(anyhow!("backend stdout closed before URL")),
        Err(_) => return Err(anyhow!("backend did not print URL in 60s")),
    };

    let authority = url
        .strip_prefix("http://")
        .and_then(|s| s.split('/').next())
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
        if let Ok(status) = exit {
            if !status.success() {
                notify::fire(
                    "DeepSeek Harness".into(),
                    format!("Backend exited with status {status}"),
                    "critical".into(),
                )
                .await;
            }
        }
    });

    Ok(url)
}