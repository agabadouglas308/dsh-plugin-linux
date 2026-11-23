#!/usr/bin/env bash
# Smoke-test a built wrapper with a fake dsh backend.
# Usage: NODE=/path/to/node ./scripts/test-wrapper-runtime.sh /path/to/dsh-desktop-linux
set -euo pipefail

BIN="${1:?usage: test-wrapper-runtime.sh /path/to/dsh-desktop-linux}"
NODE="${NODE:-node}"
TOKEN='SECRET_TOKEN_123'
TMP="$(mktemp -d)"
WRAPPER_PID=""

cleanup() {
  if [ -n "$WRAPPER_PID" ] && kill -0 "$WRAPPER_PID" 2>/dev/null; then
    kill -TERM "$WRAPPER_PID" 2>/dev/null || true
    wait "$WRAPPER_PID" 2>/dev/null || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

mkdir -p "$TMP/bin" "$TMP/runtime"
cat >"$TMP/bin/dsh" <<EOF
#!/bin/sh
exec "$NODE" -e '
const net = require("node:net")
const s = net.createServer((c) => c.end())
s.listen(0, "127.0.0.1", () => {
  console.log("dsh listening on http://127.0.0.1:" + s.address().port + "/?token=$TOKEN")
})
'
EOF
chmod +x "$TMP/bin/dsh"

export PATH="$TMP/bin:$PATH"
export XDG_RUNTIME_DIR="$TMP/runtime"
export RUST_LOG="dsh_desktop_linux=debug,dsh=debug"
export DISPLAY="${DISPLAY:-:0}"

"$BIN" >"$TMP/wrapper.log" 2>&1 &
WRAPPER_PID=$!

echo "== waiting for backend handshake"
ready=0
for _ in $(seq 1 400); do
  if grep -q 'backend ready' "$TMP/wrapper.log"; then ready=1; break; fi
  if ! kill -0 "$WRAPPER_PID" 2>/dev/null; then break; fi
  sleep 0.1
done
if [ "$ready" -ne 1 ]; then
  echo "!! wrapper never reported backend ready" >&2
  cat "$TMP/wrapper.log" >&2
  exit 1
fi

if grep -qF "$TOKEN" "$TMP/wrapper.log"; then
  echo "!! backend token leaked into logs" >&2
  grep -F "$TOKEN" "$TMP/wrapper.log" >&2
  exit 1
fi
echo "token redaction: OK"

sleep 1
if xprop -name 'DeepSeek Harness' >/dev/null 2>&1; then
  echo "window created: OK"
else
  echo "window created: inconclusive (xprop lookup failed)"
fi

echo "== second instance must be refused"
if "$BIN" >"$TMP/second.log" 2>&1; then
  echo "!! second instance unexpectedly started" >&2
  exit 1
fi
grep -q 'another dsh-desktop-linux instance is already running' "$TMP/second.log" || {
  cat "$TMP/second.log" >&2
  echo "!! no single-instance diagnostic" >&2
  exit 1
}
echo "single-instance guard: OK"

echo "== SIGTERM shutdown"
kill -TERM "$WRAPPER_PID"
set +e
wait "$WRAPPER_PID"
code=$?
set -e
WRAPPER_PID=""
if [ "$code" -ne 0 ]; then
  echo "!! wrapper exited $code on SIGTERM" >&2
  cat "$TMP/wrapper.log" >&2
  exit 1
fi
if [ -e "$XDG_RUNTIME_DIR/dsh-desktop-linux/notify.sock" ]; then
  echo "!! notify socket left behind" >&2
  exit 1
fi
echo "clean shutdown + socket cleanup: OK"

echo "== startup failure leaves no backend"
cat >"$TMP/bin/dsh" <<'EOF'
#!/bin/sh
exit 7
EOF
chmod +x "$TMP/bin/dsh"
if "$BIN" >"$TMP/fail.log" 2>&1; then
  echo "!! wrapper succeeded with a failing backend" >&2
  exit 1
fi
grep -q 'backend stdout closed before URL' "$TMP/fail.log" || {
  cat "$TMP/fail.log" >&2
  echo "!! expected startup failure diagnostic" >&2
  exit 1
}
echo "startup failure handling: OK"

echo "wrapper runtime test: PASS"
