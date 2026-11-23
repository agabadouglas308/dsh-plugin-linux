#!/usr/bin/env bash
# Exercise scripts/install-plugin.sh against a throwaway profile.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

PROFILE="$TMP/profiles/test"
mkdir -p "$PROFILE" "$TMP/bin"

# Stand-in for `dsh --dump-config`; the installer only needs to see the bundle
# name in the composed output.
cat >"$TMP/bin/dsh" <<'EOF'
#!/bin/sh
echo "bundles:"
echo "  - name: '@deepseek-ai/dsh-plugin-linux-system'"
EOF
chmod +x "$TMP/bin/dsh"

# A stale v0.1.0 patch row must be removed.
cat >"$PROFILE/cordis.patch.yml" <<'EOF'
[]
- insert:
    - id: linux-system
      name: '@deepseek-ai/dsh-plugin-linux-system'
EOF

run_installer() {
  PATH="$TMP/bin:$PATH" \
    NODE="${NODE:-node}" \
    DSH_HOME="$TMP" \
    DSH_PROFILE=test \
    "$REPO_ROOT/scripts/install-plugin.sh"
}

echo "== first install"
out="$(run_installer)"
echo "$out"
grep -q 'composition OK' <<<"$out"
grep -q 'removed the legacy linux-system row' <<<"$out"

LINK="$PROFILE/node_modules/@deepseek-ai/dsh-plugin-linux-system"
[ -L "$LINK" ]
[ "$(readlink "$LINK")" = "$REPO_ROOT/packages/extensions/plugin-linux-system" ]

NODE_BIN="${NODE:-node}"
"$NODE_BIN" -e '
const fs = require("node:fs")
const p = process.argv[1]
const m = JSON.parse(fs.readFileSync(p, "utf8"))
const name = "@deepseek-ai/dsh-plugin-linux-system"
if (!m.dsh?.profile?.bundles?.includes(name)) throw new Error("bundle not registered")
if (m.dependencies[name] !== `link:${process.argv[2]}`) throw new Error("dependency not linked")
console.log("manifest OK")
' "$PROFILE/package.json" "$REPO_ROOT/packages/extensions/plugin-linux-system"

if grep -q -- '- insert:' "$PROFILE/cordis.patch.yml"; then
  echo "!! legacy patch row survived" >&2
  exit 1
fi

echo "== second install (idempotence)"
out="$(run_installer)"
echo "$out"
grep -q 'composition OK' <<<"$out"

echo "installer test: PASS"
