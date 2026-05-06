#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOCAL_SCRIPT="$ROOT_DIR/scripts/prepare_llm_access_cloud_release.sh"
REMOTE_SCRIPT="$ROOT_DIR/scripts/activate_llm_access_cloud_release.sh"

for script in "$LOCAL_SCRIPT" "$REMOTE_SCRIPT"; do
  test -x "$script"
  bash -n "$script"
done

if command -v shellcheck >/dev/null 2>&1; then
  shellcheck "$LOCAL_SCRIPT" "$REMOTE_SCRIPT"
fi

grep -F 'CARGO_TARGET_DIR' "$LOCAL_SCRIPT" >/dev/null
grep -F 'cargo test -p llm-access-core -p llm-access-store -p llm-access' "$LOCAL_SCRIPT" >/dev/null
grep -F 'cargo clippy -p llm-access-core -p llm-access-store -p llm-access' "$LOCAL_SCRIPT" >/dev/null
grep -F 'cargo build -p llm-access --release' "$LOCAL_SCRIPT" >/dev/null
grep -F 'scp ' "$LOCAL_SCRIPT" >/dev/null
grep -F 'llm-access.latest' "$LOCAL_SCRIPT" >/dev/null

grep -F 'sudo mv -f' "$REMOTE_SCRIPT" >/dev/null
grep -F 'systemctl restart' "$REMOTE_SCRIPT" >/dev/null
grep -F 'http://127.0.0.1:19080/healthz' "$REMOTE_SCRIPT" >/dev/null
