//! Native Linux notifications via notify-rust (D-Bus session bus).
//!
//! Triggers:
//!   1. Wrapper code calls `fire(summary, body, urgency).await`
//!   2. Any local process writes a JSON line to the Unix socket at
//!      $XDG_RUNTIME_DIR/dsh-desktop-linux/notify.sock
//!      Payload: {"summary":"...","body":"...","urgency":"low|normal|critical"}
//!      The `dsh` backend and its plugins inherit DSH_NOTIFY_SOCK pointing at it.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;

/// Notification priority. Kept as a type so urgency cannot drift between the
/// wrapper, the socket payload and the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    pub fn parse(value: &str) -> Self {
        match value {
            "low" => Self::Low,
            "critical" => Self::Critical,
            _ => Self::Normal,
        }
    }
}

pub fn socket_path() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("dsh-desktop-linux").join("notify.sock")
}

/// Prepare the socket path without stealing a socket owned by a live wrapper.
///
/// A successful `connect` proves another instance is listening; in that case
/// we fail instead of unlinking its socket and splitting the notification
/// channel between two wrappers.
pub fn prepare_socket() -> Result<PathBuf> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The socket is a local control channel; keep other users out.
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    if path.exists() {
        #[cfg(unix)]
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            return Err(anyhow!(
                "another dsh-desktop-linux instance is already running (socket {})",
                path.display()
            ));
        }
        let _ = std::fs::remove_file(&path);
    }
    Ok(path)
}

pub fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
}

pub async fn listen(path: PathBuf) -> Result<()> {
    let listener = UnixListener::bind(&path).with_context(|| format!("bind {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod {}", path.display()))?;
    }
    tracing::info!(path = %path.display(), "notify socket listening");
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stream).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        match serde_json::from_str::<serde_json::Value>(&line) {
                            Ok(v) => {
                                let summary = v
                                    .get("summary")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("DeepSeek Harness");
                                let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
                                let urgency = Urgency::parse(
                                    v.get("urgency")
                                        .and_then(|x| x.as_str())
                                        .unwrap_or("normal"),
                                );
                                fire(summary.to_string(), body.to_string(), urgency).await;
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

pub async fn fire(summary: String, body: String, urgency: Urgency) {
    tokio::task::spawn_blocking(move || {
        use notify_rust::Notification;
        use notify_rust::Urgency as BusUrgency;
        let urg = match urgency {
            Urgency::Low => BusUrgency::Low,
            Urgency::Normal => BusUrgency::Normal,
            Urgency::Critical => BusUrgency::Critical,
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

#[cfg(test)]
mod tests {
    use super::Urgency;

    #[test]
    fn urgency_parse_is_strict_about_known_values() {
        assert_eq!(Urgency::parse("low"), Urgency::Low);
        assert_eq!(Urgency::parse("critical"), Urgency::Critical);
        assert_eq!(Urgency::parse("normal"), Urgency::Normal);
        assert_eq!(Urgency::parse("bogus"), Urgency::Normal);
    }
}
