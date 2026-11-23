# AGENTS.md

Guidance for coding agents working in this repository.

## What this repo is

Two deliverables for DeepSeek Harness on Linux:

- **`apps/desktop-linux/`** — Tauri 2 + Rust desktop wrapper. Spawns `dsh web`,
  opens a tokenized WebView window, and owns the tray, global shortcut, signal
  handling, backend shutdown, and the `DSH_NOTIFY_SOCK` Unix socket.
- **`packages/extensions/plugin-linux-system/`** — Cordis/TypeScript plugin.
  Registers seven `linux_*` tools (systemd, journal, clipboard, containers,
  notifications). The compiled `lib/` is checked in and must be rebuilt when
  `src/index.ts` changes.

## Layout

```
apps/desktop-linux/src/lib.rs       wrapper lifecycle (tray, signals, backend)
apps/desktop-linux/src/notify.rs    D-Bus notifications + notify socket
apps/desktop-linux/tauri.conf.json  bundling, deps, window/security config
packages/extensions/plugin-linux-system/src/index.ts  tool definitions
packages/extensions/plugin-linux-system/lib/          checked-in build output
scripts/install-plugin.sh           links the bundle into a dsh profile
docs/agents/                        issue tracker, triage labels, domain docs
```

## Commands

```bash
pnpm install
pnpm typecheck                 # tsc --noEmit for the plugin
pnpm build:plugin              # rebuild lib/ from src/
cd apps/desktop-linux && cargo fmt --check && cargo check
cd apps/desktop-linux && cargo clippy -- -D warnings && cargo test
./scripts/install-plugin.sh    # install into $DSH_HOME/profiles/web
```

Build prerequisites: Node.js >= 24.9, pnpm 10, Rust 1.90, Tauri 2 Linux
dependencies (WebKitGTK 4.1, GTK 3, libayatana-appindicator3, libnotify).
Release bundles: `cd apps/desktop-linux && cargo tauri build --bundles deb,rpm,appimage`.

## Verification

Do not call work done on the wrapper without `cargo fmt --check`, `cargo check`,
`cargo clippy -- -D warnings`, and `cargo test`. Do not call work done on the
plugin without `pnpm typecheck` and `pnpm build:plugin`, and commit the rebuilt
`lib/`. For installer changes, run `./scripts/test-install-plugin.sh` (fresh profile,
legacy-patch cleanup, idempotence) and confirm `dsh --profile web --dump-config`
composes the bundle.

## Agent skills

### Issue tracker

Issues live in GitHub Issues in `agabadouglas308/dsh-plugin-linux`. See `docs/agents/issue-tracker.md`.

### Triage labels

Default five-role vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`; `bug`/`enhancement` are the categories. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
