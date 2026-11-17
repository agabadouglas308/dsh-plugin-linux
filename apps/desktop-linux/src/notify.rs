//! Native Linux notifications via notify-rust (D-Bus session bus).
//!
//! Triggers:
//!   1. Wrapper code calls `fire(summary, body, urgency).await`
//!   2. Any local process writes a JSON line to the Unix socket at
//!      $XDG_RUNTIME_DIR/dsh-desktop-linux/notify.sock
//!      Payload: {"summary":"...","body":"...","urgency":"low|normal|critical"}
//!      The `dsh` backend and its plugins inherit DSH_NOTIFY_SOCK pointing at it.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;

pub fn socket_path() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("dsh-desktop-linux").join("notify.sock")
}

pub fn prepare_socket() -> Result<PathBuf> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create {}", dir.display()))?;
    }
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    Ok(path)
}

pub fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

pub async fn listen(path: PathBuf) -> Result<()> {
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind {}", path.display()))?;
    tracing::info!(path = %path.display(), "notify socket listening");
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stream).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        match serde_json::from_str::<serde_json::Value>(&line) {
                            Ok(v) => {
                                let summary = v.get("summary").and_then(|x| x.as_str()).unwrap_or("DeepSeek Harness");
                                let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
                                let urgency = v.get("urgency").and_then(|x| x.as_str()).unwrap_or("normal");
                                fire(summary.to_string(), body.to_string(), urgency.to_string()).await;
                            }
                            Err(e) => tracing::warn!(error = %e, "invalid notify payload"),
                        }
                    }
                });
            }
            Err(e) => tracing::warn!(error = %e, "notify accept failed"),
        }
    }
}

pub async fn fire(summary: String, body: String, urgency: String) {
    tokio::task::spawn_blocking(move || {
        use notify_rust::{Notification, Urgency};
        let urg = match urgency.as_str() {
            "low" => Urgency::Low,
            "critical" => Urgency::Critical,
            _ => Urgency::Normal,
        };
        match Notification::new()
            .appname("DeepSeek Harness")
            .summary(&summary)
            .body(&body)
            .urgency(urg)
            .timeout(notify_rust::Timeout::Milliseconds(5000))
            .show()
        {
            Ok(_) => tracing::debug!(summary = %summary, "notification fired"),
            Err(e) => tracing::warn!(error = %e, "notification send failed"),
        }
    })
    .await
    .ok();
}