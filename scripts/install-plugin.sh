#!/usr/bin/env bash
# Install the linux-system plugin into a dsh profile.
#
# The plugin is a dsh *bundle*: it ships `cordis.patch.yml`, and the profile
# loader applies that layer when the package is listed in `dsh.profile.bundles`.
# We link the checkout into the profile and register it there, which is the same
# state `dsh plugin add <path>` produces without requiring pnpm.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PLUGIN_DIR="$REPO_ROOT/packages/extensions/plugin-linux-system"
PKG_NAME='@deepseek-ai/dsh-plugin-linux-system'
PROFILE_NAME="${DSH_PROFILE:-web}"
PROFILE_DIR="${DSH_HOME:-$HOME/.dsh}/profiles/$PROFILE_NAME"
NODE_MODULES="$PROFILE_DIR/node_modules/@deepseek-ai"
LINK="$NODE_MODULES/dsh-plugin-linux-system"
PATCH_FILE="$PROFILE_DIR/cordis.patch.yml"

die() {
  echo "!! $*" >&2
  exit 1
}

[ -d "$PROFILE_DIR" ] || die "profile not found: $PROFILE_DIR
   Start dsh once first (dsh web), or set DSH_PROFILE=<name>."
[ -f "$PLUGIN_DIR/lib/index.mjs" ] || die "pre-built plugin lib/ missing at $PLUGIN_DIR/lib/
   Rebuild it from source: cd \"$PLUGIN_DIR\" && pnpm install && pnpm build"
[ -f "$PLUGIN_DIR/cordis.patch.yml" ] || die "bundle patch missing: $PLUGIN_DIR/cordis.patch.yml"

NODE_BIN="${NODE:-node}"
command -v "$NODE_BIN" >/dev/null 2>&1 || die "node not found; install Node.js >= 24.9.0"

echo "[1/3] linking $PKG_NAME into $NODE_MODULES ..."
mkdir -p "$NODE_MODULES"
if [ -L "$LINK" ]; then
  rm -f "$LINK"
elif [ -e "$LINK" ]; then
  die "$LINK exists and is not a symlink; remove it and rerun"
fi
ln -s "$PLUGIN_DIR" "$LINK"

echo "[2/3] registering the bundle in $PROFILE_DIR/package.json ..."
PROFILE_DIR="$PROFILE_DIR" PLUGIN_DIR="$PLUGIN_DIR" PKG_NAME="$PKG_NAME" PATCH_FILE="$PATCH_FILE" \
  "$NODE_BIN" <<'NODE'
const fs = require('node:fs')
const path = require('node:path')

const { PROFILE_DIR, PLUGIN_DIR, PKG_NAME, PATCH_FILE } = process.env
const manifestPath = path.join(PROFILE_DIR, 'package.json')

let manifest
if (fs.existsSync(manifestPath)) {
  manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8'))
} else {
  // dsh creates this on first boot; recreate the minimal shape if it is absent.
  manifest = {
    name: `dsh-profile-${path.basename(PROFILE_DIR)}`,
    private: true,
    dependencies: {},
    dsh: { profile: { bundles: [] } },
  }
}
manifest.dependencies ??= {}
manifest.dependencies[PKG_NAME] = `link:${PLUGIN_DIR}`
manifest.dsh ??= {}
manifest.dsh.profile ??= {}
const bundles = Array.isArray(manifest.dsh.profile.bundles) ? manifest.dsh.profile.bundles : []
manifest.dsh.profile.bundles = [...bundles.filter(name => name !== PKG_NAME), PKG_NAME]
fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`)
console.log(`      ${path.relative(PROFILE_DIR, manifestPath)}: bundles = ${manifest.dsh.profile.bundles.join(', ')}`)

// Older versions of this installer appended a `- insert:` row after the stock
// `[]` sequence, producing invalid YAML that made dsh fail to boot. The bundle
// layer now owns that row, so drop any legacy copy.
if (fs.existsSync(PATCH_FILE)) {
  const lines = fs.readFileSync(PATCH_FILE, 'utf8').split('\n')
  const kept = []
  let changed = false
  for (let i = 0; i < lines.length; i += 1) {
    const isLegacyInsert = lines[i].trim() === '- insert:'
      && lines[i + 1]?.trim() === '- id: linux-system'
      && lines[i + 2]?.includes(PKG_NAME)
    if (isLegacyInsert) {
      i += 2
      if (kept.at(-1)?.trim() === '') kept.pop()
      changed = true
      continue
    }
    kept.push(lines[i])
  }
  if (changed) {
    const meaningful = kept.filter(line => line.trim() !== '' && !line.trim().startsWith('#'))
    if (meaningful.length === 0) kept.push('[]')
    fs.writeFileSync(PATCH_FILE, `${kept.join('\n').replace(/\n+$/, '')}\n`)
    console.log(`      removed the legacy linux-system row from ${path.basename(PATCH_FILE)}`)
  }
}
NODE

echo "[3/3] verifying the composed profile ..."
if command -v dsh >/dev/null 2>&1; then
  if ! dump="$(dsh --profile "$PROFILE_NAME" --dump-config 2>&1)"; then
    echo "!! dsh could not compose the profile:"
    echo "$dump" | tail -n 8 >&2
    exit 1
  fi
  if ! grep -q "name: '$PKG_NAME'" <<<"$dump"; then
    die "bundle registered but dsh did not compose it; check the output of: dsh --profile $PROFILE_NAME --dump-config"
  fi
  echo "      composition OK"
else
  echo "      dsh not on PATH — skipped (restart the app after installing dsh)"
fi

echo ""
echo "✅ installed into profile: $PROFILE_NAME"
echo "   Restart dsh-desktop-linux, then open Settings → Plugins → search 'linux-system'."
echo "   Status should show: Running"
