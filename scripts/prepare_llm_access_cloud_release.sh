#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REMOTE_SCRIPT="$ROOT_DIR/scripts/activate_llm_access_cloud_release.sh"

DEFAULT_TARGET_DIR="/mnt/wsl/data4tb/static-flow-data/cargo-target/static_flow"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$DEFAULT_TARGET_DIR}"
GCP_USER="${GCP_USER:-ts_user}"
GCP_HOST="${GCP_HOST:-35.241.86.154}"
GCP_SSH_KEY="${GCP_SSH_KEY:-$HOME/.ssh/google_compute_engine}"
GCP_DEST="${GCP_DEST:-$GCP_USER@$GCP_HOST}"
REMOTE_RELEASE_DIR="${REMOTE_RELEASE_DIR:-/home/$GCP_USER/staticflow-llm-access-release}"
BUILD_JOBS="${BUILD_JOBS:-4}"
ALLOW_DIRTY="${ALLOW_DIRTY:-0}"

log() {
  printf '[llm-access-release] %s\n' "$*"
}

fail() {
  printf '[llm-access-release][ERROR] %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

shell_quote() {
  printf '%q' "$1"
}

for cmd in cargo curl df git scp sha256sum ssh; do
  require_cmd "$cmd"
done

[[ -x "$REMOTE_SCRIPT" ]] || fail "remote activation script is not executable: $REMOTE_SCRIPT"
[[ -r "$GCP_SSH_KEY" ]] || fail "SSH key is not readable: $GCP_SSH_KEY"

cd "$ROOT_DIR"

if [[ "$ALLOW_DIRTY" != "1" ]] && [[ -n "$(git status --porcelain)" ]]; then
  git status --short >&2
  fail "working tree is dirty; commit first or run with ALLOW_DIRTY=1"
fi

df -h /mnt/wsl/data4tb >/dev/null

export CARGO_TARGET_DIR

log "running llm-access test suite"
cargo test -p llm-access-core -p llm-access-store -p llm-access --jobs "$BUILD_JOBS"

log "running llm-access clippy"
cargo clippy -p llm-access-core -p llm-access-store -p llm-access --jobs "$BUILD_JOBS" -- -D warnings

log "building release binary"
cargo build -p llm-access --release --jobs "$BUILD_JOBS"

BIN="$CARGO_TARGET_DIR/release/llm-access"
[[ -x "$BIN" ]] || fail "built binary not found or not executable: $BIN"

GIT_COMMIT="$(git rev-parse HEAD)"
GIT_SHORT="$(git rev-parse --short=12 HEAD)"
RELEASE_ID="${RELEASE_ID:-$(date -u +%Y%m%dT%H%M%SZ)-$GIT_SHORT}"
OUT_DIR="${LLM_ACCESS_RELEASE_OUT:-$ROOT_DIR/tmp/llm-access-cloud-release/$RELEASE_ID}"
STAGED_BIN="$OUT_DIR/llm-access.$RELEASE_ID"
MANIFEST="$OUT_DIR/release.$RELEASE_ID.env"
SHA_FILE="$OUT_DIR/SHA256SUMS.$RELEASE_ID"

mkdir -p "$OUT_DIR"
cp "$BIN" "$STAGED_BIN"
chmod 0755 "$STAGED_BIN"
BIN_SHA="$(sha256sum "$STAGED_BIN" | awk '{print $1}')"
printf '%s  %s\n' "$BIN_SHA" "llm-access.$RELEASE_ID" >"$SHA_FILE"

cat >"$MANIFEST" <<EOF
release_id=$RELEASE_ID
git_commit=$GIT_COMMIT
git_short=$GIT_SHORT
built_at_utc=$(date -u +%FT%TZ)
sha256=$BIN_SHA
binary=llm-access.$RELEASE_ID
EOF

SSH_OPTS=(-i "$GCP_SSH_KEY" -o IdentitiesOnly=yes -o BatchMode=yes)
REMOTE_RELEASE_DIR_Q="$(shell_quote "$REMOTE_RELEASE_DIR")"
REMOTE_SCRIPT_NAME="$(basename "$REMOTE_SCRIPT")"

log "checking GCP target: $GCP_DEST"
ssh "${SSH_OPTS[@]}" "$GCP_DEST" "
  set -e
  mkdir -p $REMOTE_RELEASE_DIR_Q
  systemctl is-active juicefs-llm-access.service >/dev/null
  findmnt -T /mnt/llm-access >/dev/null
  curl -fsS http://127.0.0.1:19080/healthz >/dev/null || echo '[llm-access-release][WARN] current llm-access health check failed; staging continues'
"

log "uploading release $RELEASE_ID to $GCP_DEST:$REMOTE_RELEASE_DIR"
scp "${SSH_OPTS[@]}" \
  "$STAGED_BIN" \
  "$MANIFEST" \
  "$SHA_FILE" \
  "$REMOTE_SCRIPT" \
  "$GCP_DEST:$REMOTE_RELEASE_DIR/"

log "updating remote latest pointers"
ssh "${SSH_OPTS[@]}" "$GCP_DEST" "
  set -e
  cd $REMOTE_RELEASE_DIR_Q
  chmod 0755 $REMOTE_SCRIPT_NAME llm-access.$RELEASE_ID
  sha256sum -c SHA256SUMS.$RELEASE_ID
  ln -sfn llm-access.$RELEASE_ID llm-access.latest
  ln -sfn release.$RELEASE_ID.env release.latest.env
"

cat <<EOF

Prepared llm-access cloud release:
  release_id: $RELEASE_ID
  git_commit: $GIT_COMMIT
  sha256: $BIN_SHA
  remote_dir: $REMOTE_RELEASE_DIR

Run this on GCP to activate it:
  ssh -i "$GCP_SSH_KEY" -o IdentitiesOnly=yes "$GCP_DEST"
  $REMOTE_RELEASE_DIR/$REMOTE_SCRIPT_NAME

Or activate directly from local:
  ssh -i "$GCP_SSH_KEY" -o IdentitiesOnly=yes "$GCP_DEST" '$REMOTE_RELEASE_DIR/$REMOTE_SCRIPT_NAME'
EOF
