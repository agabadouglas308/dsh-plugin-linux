# DeepSeek Harness — Linux Desktop + System Plugin

Native Linux desktop wrapper for [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) plus a Cordis plugin that adds native system capabilities (systemd, journalctl, clipboard, containers, notifications).

## What you get

### 1. Desktop wrapper (`apps/desktop-linux/`)

Tauri 2 + Rust + WebKitGTK. Manages `dsh web` and opens a native window pointed at its tokenized URL.

- Spawns `dsh web --port 0 --no-open`, reads tokenized URL from stdout
- System tray: Show Window / Quit
- Close-to-tray: X button hides, backend keeps running
- Global shortcut `Ctrl+Shift+D` (X11/XWayland) + SIGUSR1 fallback for Wayland
- D-Bus notifications + Unix socket bridge for plugins (`$DSH_NOTIFY_SOCK`)
- Clean shutdown: SIGTERM → SIGKILL to the whole backend process group

### 2. System plugin (`packages/extensions/plugin-linux-system/`)

Cordis plugin registering **7 tools** with the agent:

- `linux_systemd_status(unit, scope?)` — structured unit status
- `linux_systemd_action(unit, action, scope?)` — start/stop/restart/enable/disable
- `linux_journal(unit?, lines?, since?)` — structured journalctl entries
- `linux_clipboard_get()` — read clipboard (Wayland-first, X11 fallback)
- `linux_clipboard_set(text)` — write clipboard
- `linux_containers()` — list running containers (podman/docker)
- `linux_notify(summary, body?, urgency?)` — native notification

## Prerequisites

1. **Node.js >= 22** — `sudo dnf install -y nodejs` (Fedora) or `sudo apt install -y nodejs` (Debian/Ubuntu)
2. **DeepSeek Harness backend** — `npm install -g @deepseek-ai/dsh`. Verify with `command -v dsh`
3. **DeepSeek account with API quota** — the agent needs it to respond; plugin loads without it

## Installation

AppImage (any Linux distro):

    chmod +x DeepSeek-Harness_0.1.0_amd64.AppImage
    ./DeepSeek-Harness_0.1.0_amd64.AppImage

Fedora / RHEL / CentOS:

    sudo dnf install -y DeepSeek-Harness-0.1.0-1.x86_64.rpm

Debian / Ubuntu:

    sudo dpkg -i DeepSeek-Harness_0.1.0_amd64.deb
    sudo apt-get install -f

First launch: `dsh-desktop-linux` — wait 20-40s for backend, then window opens.

## Desktop integration

- **Tray** — Left-click shows window; Right-click opens Show/Quit menu
- **Close X** — Hides to tray, backend keeps running
- **Quit** — Tray → Quit → clean shutdown of all processes
- **Ctrl+Shift+D** — Toggle window (X11/XWayland)
- **Notifications** — D-Bus toasts + Unix socket bridge for plugins

### Wayland shortcut setup

On GNOME/Wayland, `Ctrl+Shift+D` may silently no-op. Bind a GNOME custom shortcut instead:

    mkdir -p ~/.local/bin
    printf '#!/bin/sh\npid=$(pgrep -u "$USER" -f dsh-desktop-linux | head -1)\n[ -z "$pid" ] && exit 1\nkill -USR1 "$pid"\n' > ~/.local/bin/dsh-harness-toggle
    chmod +x ~/.local/bin/dsh-harness-toggle

Then in GNOME Settings → Keyboard → Custom Shortcuts, bind `Ctrl+Shift+D` to `~/.local/bin/dsh-harness-toggle`.

## Plugin installation (opt-in)

    git clone https://github.com/agabadouglas308/dsh-plugin-linux.git
    cd dsh-plugin-linux
    ./scripts/install-plugin.sh

No `pnpm install` needed — the plugin ships pre-built (`lib/index.mjs`).

Restart `dsh-desktop-linux`. Verify in **Settings → Built-in plugins → search `linux-system`**. Status should show **Running**.

Test: in the chat panel, ask the agent "What is the status of sshd.service? Use the linux_systemd_status tool."

## Troubleshooting

- **Backend failed to start: no dsh backend found** — run `npm install -g @deepseek-ai/dsh`, ensure `command -v dsh` resolves
- **Window blank for >60s** — check terminal for `dsh.stderr` lines
- **ACCOUNT_QUOTA in chat** — DeepSeek API quota exhausted, unrelated to wrapper
- **Ctrl+Shift+D does nothing (Wayland)** — use the SIGUSR1 fallback above
- **Plugin not in Settings** — rerun `./scripts/install-plugin.sh`, restart app

## Development

    git clone https://github.com/deepseek-ai/deepseek-harness.git
    cd deepseek-harness/apps/desktop-linux
    cargo build              # dev build
    cargo tauri build --bundles deb,rpm,appimage

Environment variables:

- `DSH_REPO` — path to a deepseek-harness checkout (dev mode; overrides `dsh` on PATH)
- `DSH_PROFILE` — which profile to install the plugin into (default: `web`)
- `DSH_HOME` — DSH home root (default: `~/.dsh`)
- `RUST_LOG=dsh_desktop_linux=debug` — verbose wrapper logging

## License

MIT — see LICENSE.
