#!/usr/bin/env bash
# Install the linux-system plugin into a dsh profile.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PLUGIN_DIR="$REPO_ROOT/packages/extensions/plugin-linux-system"
PROFILE_NAME="${DSH_PROFILE:-web}"
PROFILE_DIR="${DSH_HOME:-$HOME/.dsh}/profiles/$PROFILE_NAME"
NODE_MODULES="$PROFILE_DIR/node_modules/@deepseek-ai"
PATCH_FILE="$PROFILE_DIR/cordis.patch.yml"

if [ ! -d "$PROFILE_DIR" ]; then
  echo "!! profile not found: $PROFILE_DIR"
  echo "   Start dsh once first (dsh web), or set DSH_PROFILE=<name>."
  exit 1
fi

if [ ! -f "$PLUGIN_DIR/lib/index.mjs" ]; then
  echo "!! pre-built plugin lib/ missing at $PLUGIN_DIR/lib/"
  echo "   This repo ships pre-built lib/. If missing, rebuild from source:"
  echo "   cd $PLUGIN_DIR && pnpm install && pnpm build"
  exit 1
fi

echo "[1/2] symlinking into $NODE_MODULES ..."
mkdir -p "$NODE_MODULES"
ln -sfn "$PLUGIN_DIR" "$NODE_MODULES/dsh-plugin-linux-system"

echo "[2/2] registering in $PATCH_FILE ..."
if grep -q "id: linux-system" "$PATCH_FILE" 2>/dev/null; then
  echo "    already registered — skipping"
else
  [ -f "$PATCH_FILE" ] || { mkdir -p "$(dirname "$PATCH_FILE")"; printf '# dsh profile patch layer\n' > "$PATCH_FILE"; }
  printf '\n- insert:\n    - id: linux-system\n      name: '"'"'@deepseek-ai/dsh-plugin-linux-system'"'"'\n' >> "$PATCH_FILE"
  echo "    appended"
fi

echo ""
echo "✅ installed into profile: $PROFILE_NAME"
echo "   Verify: restart dsh-desktop-linux, then Settings → Built-in plugins → search 'linux-system'"
echo "   Status should show: Running"
