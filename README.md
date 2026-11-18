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

1. **Node.js >= 24.9** — `sudo dnf install -y nodejs` (Fedora) or `sudo apt install -y nodejs` (Debian/Ubuntu). DeepSeek Harness 0.2.x declares `node >= 24.9`; Node 22 is **not** supported.
2. **DeepSeek Harness backend** — `npm install -g @deepseek-ai/dsh`. Verify with `command -v dsh`.
3. **DeepSeek account with API quota** — the agent needs it to respond; plugin loads without it
4. **pnpm 10** (optional) — only needed for `dsh plugin add/remove`; `scripts/install-plugin.sh` does not require it

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

The plugin ships pre-built (`lib/index.mjs`) and is also a proper dsh **bundle**
(`package.json` → `dsh.bundle` + `cordis.patch.yml`). The installer links the
checkout into the profile, adds it to `dsh.profile.bundles`, removes the broken
patch row written by v0.1.0 installers, and verifies the composed profile with
`dsh --profile web --dump-config`.

It does **not** run `pnpm install`, so no network access or pnpm is required.
Profiles are read from `$DSH_HOME/profiles/<name>` (default `web`).

Restart `dsh-desktop-linux`. Verify in **Settings → Plugins → search `linux-system`**. Status should show **Running**.

Test: in the chat panel, ask the agent "What is the status of sshd.service? Use the linux_systemd_status tool."

## Troubleshooting

- **Backend failed to start: no dsh backend found** — run `npm install -g @deepseek-ai/dsh`, ensure `command -v dsh` resolves. GUI launches inherit a minimal PATH; the wrapper now merges your login-shell PATH, but `~/.npm-global/bin` and nvm paths must be exported from your shell profile.
- **`failed to parse overlay ... cordis.patch.yml`** — a v0.1.0 installer appended a `- insert:` row after the stock `[]`, which is invalid YAML. Rerun `./scripts/install-plugin.sh`; it removes the legacy row and registers the bundle instead.
- **Window blank for >120s** — check terminal for `dsh.stderr` lines. Set `DSH_STARTUP_TIMEOUT_SECS=240` for a slow first boot.
- **ACCOUNT_QUOTA in chat** — DeepSeek API quota exhausted, unrelated to wrapper
- **Ctrl+Shift+D does nothing (Wayland)** — use the SIGUSR1 fallback above
- **No tray icon, or window close quits** — `libayatana-appindicator3` is missing; install it to restore close-to-tray.
- **Plugin not in Settings** — rerun `./scripts/install-plugin.sh`, check its verification step, then restart the app

## Development

    git clone https://github.com/agabadouglas308/dsh-plugin-linux.git
    cd dsh-plugin-linux
    pnpm install                  # Node 24.11+ required by tsdown; installs the type definitions
    pnpm build:plugin             # rebuild packages/extensions/plugin-linux-system/lib
    pnpm typecheck

    cd apps/desktop-linux
    cargo build                   # dev build (Rust 1.90+)
    cargo tauri build --bundles deb,rpm,appimage   # needs tauri-cli ^2 and WebKitGTK dev packages

To work against a source checkout of the harness, set `DSH_REPO=/path/to/deepseek-harness`.

Environment variables:

- `DSH_REPO` — path to a deepseek-harness checkout (dev mode; overrides `dsh` on PATH)
- `DSH_PROFILE` — which profile to install the plugin into (default: `web`)
- `DSH_HOME` — DSH home root (default: `~/.dsh`)
- `DSH_STARTUP_TIMEOUT_SECS` — backend URL wait budget (default: `120`)
- `RUST_LOG=dsh_desktop_linux=debug` — verbose wrapper logging

## License

MIT — see LICENSE.
