#!/usr/bin/env bash
set -euo pipefail

RELEASE_DIR="${LLM_ACCESS_RELEASE_DIR:-$HOME/staticflow-llm-access-release}"
SERVICE="${LLM_ACCESS_SERVICE:-llm-access.service}"
INSTALL_PATH="${LLM_ACCESS_INSTALL_PATH:-/usr/local/bin/llm-access}"
BACKUP_DIR="${LLM_ACCESS_BACKUP_DIR:-/usr/local/bin/staticflow-backups}"
HEALTH_URL="${LLM_ACCESS_HEALTH_URL:-http://127.0.0.1:19080/healthz}"
VERSION_URL="${LLM_ACCESS_VERSION_URL:-http://127.0.0.1:19080/version}"
JOURNAL_LINES="${JOURNAL_LINES:-80}"
STAGED_BIN="${1:-$RELEASE_DIR/llm-access.latest}"
MANIFEST="${LLM_ACCESS_RELEASE_MANIFEST:-$RELEASE_DIR/release.latest.env}"

log() {
  printf '[llm-access-activate] %s\n' "$*"
}

fail() {
  printf '[llm-access-activate][ERROR] %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

manifest_value() {
  local key="$1"
  [[ -f "$MANIFEST" ]] || return 0
  sed -n "s/^${key}=//p" "$MANIFEST" | tail -n 1
}

wait_for_health() {
  local attempt
  for attempt in $(seq 1 30); do
    if curl -fsS "$HEALTH_URL" >/dev/null; then
      return 0
    fi
    sleep 1
  done
  return 1
}

for cmd in awk curl findmnt install sed seq sha256sum sudo systemctl; do
  require_cmd "$cmd"
done

[[ -f "$STAGED_BIN" ]] || fail "staged binary not found: $STAGED_BIN"
[[ -r "$STAGED_BIN" ]] || fail "staged binary is not readable: $STAGED_BIN"

expected_sha="$(manifest_value sha256 || true)"
actual_sha="$(sha256sum "$STAGED_BIN" | awk '{print $1}')"
if [[ -n "$expected_sha" && "$actual_sha" != "$expected_sha" ]]; then
  fail "staged binary sha256 mismatch: expected $expected_sha, got $actual_sha"
fi

release_id="$(manifest_value release_id || true)"
git_commit="$(manifest_value git_commit || true)"

log "release_id=${release_id:-unknown}"
log "git_commit=${git_commit:-unknown}"
log "staged_binary=$STAGED_BIN"
log "staged_sha256=$actual_sha"

systemctl is-active juicefs-llm-access.service >/dev/null || fail "juicefs-llm-access.service is not active"
findmnt -T /mnt/llm-access >/dev/null || fail "/mnt/llm-access is not mounted"

if systemctl is-active "$SERVICE" >/dev/null; then
  log "$SERVICE is active before activation"
  curl -fsS "$HEALTH_URL" >/dev/null || log "pre-activation health check failed; continuing with restart"
else
  log "$SERVICE is not active before activation; continuing with install"
fi

timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
backup_path="$BACKUP_DIR/llm-access.$timestamp"
tmp_install="$INSTALL_PATH.next-$timestamp"

log "backing up current binary to $backup_path"
sudo install -d -m 0755 "$BACKUP_DIR"
if sudo test -e "$INSTALL_PATH"; then
  sudo cp -a "$INSTALL_PATH" "$backup_path"
else
  log "current binary does not exist; no backup created"
fi

log "installing staged binary to $INSTALL_PATH"
sudo install -o root -g root -m 0755 "$STAGED_BIN" "$tmp_install"
sudo mv -f "$tmp_install" "$INSTALL_PATH"

installed_sha="$(sudo sha256sum "$INSTALL_PATH" | awk '{print $1}')"
if [[ "$installed_sha" != "$actual_sha" ]]; then
  if sudo test -e "$backup_path"; then
    sudo cp -a "$backup_path" "$INSTALL_PATH"
  fi
  fail "installed binary sha256 mismatch: expected $actual_sha, got $installed_sha"
fi

log "restarting $SERVICE"
sudo systemctl restart "$SERVICE"

if ! wait_for_health; then
  sudo systemctl status "$SERVICE" --no-pager -l || true
  sudo journalctl -u "$SERVICE" -n "$JOURNAL_LINES" --no-pager -l || true
  fail "health check failed after restart; rollback backup: $backup_path"
fi

log "activation succeeded"
curl -fsS "$HEALTH_URL"
printf '\n'
curl -fsS "$VERSION_URL"
printf '\n'
systemctl show "$SERVICE" -p ActiveState -p SubState -p MainPID -p ExecMainStartTimestamp -p NRestarts --no-pager

cat <<EOF

Installed llm-access release:
  release_id: ${release_id:-unknown}
  git_commit: ${git_commit:-unknown}
  sha256: $installed_sha
  backup: $backup_path

Rollback command if needed:
  sudo cp -a "$backup_path" "$INSTALL_PATH" && sudo systemctl restart "$SERVICE"
EOF
