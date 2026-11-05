#!/usr/bin/env bash

set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=/opt/nullnet
BACKUP=$ROOT/previous
NEXT_BACKUP=$ROOT/previous.new
declare -a components=() packages=()

[[ $# -eq 0 ]] || { echo "prepare.sh takes no arguments" >&2; exit 2; }
[[ $EUID -eq 0 ]] || { echo "Run as root" >&2; exit 1; }

mkdir -p "$ROOT"
exec 9>"$ROOT/deploy.lock"
flock -n 9 || { echo "Another deployment command is running" >&2; exit 1; }

for component in server proxy client; do
  if systemctl is-active --quiet "nullnet-$component.service"; then
    components+=("$component")
  fi
done
((${#components[@]})) || { echo "No active Nullnet services found" >&2; exit 1; }
requested=$(printf '%s\n' "${components[@]}")
echo "Components: ${components[*]}"

git -C "$REPO" diff --quiet
git -C "$REPO" diff --cached --quiet
git -C "$REPO" fetch origin main
target=$(git -C "$REPO" rev-parse 'FETCH_HEAD^{commit}')
current=$(git -C "$REPO" rev-parse 'HEAD^{commit}')
echo "Preparing ${current:0:12} -> ${target:0:12}"
git -C "$REPO" merge-base --is-ancestor "$current" "$target" || {
  echo "Current commit is not an ancestor of origin/main" >&2
  exit 1
}

already_prepared=true
[[ -f $BACKUP/commit && -f $BACKUP/target && -f $BACKUP/components ]] || already_prepared=false
if $already_prepared; then
  [[ $(<"$BACKUP/target") == "$target" ]] || already_prepared=false
  [[ $(<"$BACKUP/components") == "$requested" ]] || already_prepared=false
  git -C "$REPO" cat-file -e "$(<"$BACKUP/commit")^{commit}" || already_prepared=false
fi
for component in "${components[@]}"; do
  binary="$REPO/target/release/nullnet-$component"
  hash_file="$BACKUP/prepared-$component.sha256"
  if [[ ! -x $binary || ! -f $hash_file || ! -x $BACKUP/bin/nullnet-$component ]] ||
    [[ $(sha256sum "$binary" | cut -d' ' -f1) != $(<"$hash_file") ]]; then
    already_prepared=false
  fi
done
if $already_prepared && [[ " ${components[*]} " == *" server "* ]] &&
  ! git -C "$REPO" diff --quiet "$(<"$BACKUP/commit")" "$target" -- \
    members/nullnet-server/src/db/migrations; then
  [[ -f $BACKUP/database && -f $BACKUP/database-path ]] || already_prepared=false
fi
if $already_prepared; then
  echo "already prepared ${target:0:12}"
  exit 0
fi

reuse_backup=false
if [[ -f $NEXT_BACKUP/target && -f $NEXT_BACKUP/components ]] &&
  [[ $(<"$NEXT_BACKUP/target") == "$target" ]] &&
  [[ $(<"$NEXT_BACKUP/components") == "$requested" ]]; then
  reuse_backup=true
fi
if $reuse_backup; then
  for component in "${components[@]}"; do
    [[ -x $NEXT_BACKUP/bin/nullnet-$component ]] || reuse_backup=false
  done
fi
if ! $reuse_backup; then
  echo "Saving rollback binaries"
  rm -rf "$NEXT_BACKUP"
  mkdir -p "$NEXT_BACKUP/bin"
  printf '%s\n' "$current" >"$NEXT_BACKUP/commit"
  printf '%s\n' "$target" >"$NEXT_BACKUP/target"
  printf '%s\n' "${components[@]}" >"$NEXT_BACKUP/components"
  for component in "${components[@]}"; do
    binary="$REPO/target/release/nullnet-$component"
    [[ -x $binary ]] || { echo "Missing live binary: $binary" >&2; exit 1; }
    install -m 755 "$binary" "$NEXT_BACKUP/bin/nullnet-$component"
  done
else
  echo "Reusing rollback backup from the failed preparation"
fi

restore_preparation() {
  status=$?
  trap - EXIT
  if ((status != 0)); then
    set +e
    echo "Preparation failed; restoring source and binaries" >&2
    for component in "${components[@]}"; do
      install -m 755 "$NEXT_BACKUP/bin/nullnet-$component" \
        "$REPO/target/release/nullnet-$component"
    done
    if [[ $(git -C "$REPO" rev-parse 'HEAD^{commit}') != "$current" ]]; then
      git -C "$REPO" switch --detach "$current"
    fi
  fi
  exit "$status"
}
trap restore_preparation EXIT

backup_commit=$(<"$NEXT_BACKUP/commit")
if [[ " ${components[*]} " == *" server "* ]] &&
  ! git -C "$REPO" diff --quiet "$backup_commit" "$target" -- \
    members/nullnet-server/src/db/migrations; then
  if [[ -f $NEXT_BACKUP/database && -f $NEXT_BACKUP/database-path ]]; then
    echo "Reusing database snapshot from the failed preparation"
  else
    echo "Saving database snapshot"
    rm -f "$NEXT_BACKUP/database" "$NEXT_BACKUP/database-path"
    command -v python3 >/dev/null || { echo "python3 is required for database backup" >&2; exit 1; }
    database=$(sed -n 's/^DATABASE_URL=//p' "$REPO/members/nullnet-server/.env" | tail -1)
    database=${database:-/var/nullnet/data/nullnet.db}
    [[ -f $database ]] || { echo "Database not found: $database" >&2; exit 1; }
    python3 - "$database" "$NEXT_BACKUP/database" <<'PY'
import sqlite3
import sys

source = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
backup = sqlite3.connect(sys.argv[2])
source.backup(backup)
result = backup.execute("PRAGMA quick_check").fetchone()
source.close()
backup.close()
if result != ("ok",):
    raise SystemExit(f"database backup failed quick_check: {result}")
PY
    chmod 600 "$NEXT_BACKUP/database"
    printf '%s\n' "$database" >"$NEXT_BACKUP/database-path"
  fi
fi

git -C "$REPO" switch main
git -C "$REPO" merge --ff-only "$target"
[[ $(git -C "$REPO" rev-parse HEAD) == "$target" ]] || {
  echo "main is newer than the prepared target; run prepare again" >&2
  exit 1
}
echo "Building: ${components[*]}"
for component in "${components[@]}"; do
  case "$component" in
    server) packages+=(-p nullnet-server) ;;
    proxy) packages+=(-p nullnet-proxy) ;;
  esac
done
if ((${#packages[@]})); then
  (cd "$REPO" && cargo build --locked --release "${packages[@]}")
fi
if [[ " ${components[*]} " == *" client "* ]]; then
  (cd "$REPO" && cargo xtask build --release)
fi

for component in "${components[@]}"; do
  sha256sum "$REPO/target/release/nullnet-$component" | cut -d' ' -f1 \
    >"$NEXT_BACKUP/prepared-$component.sha256"
done
rm -rf "$BACKUP"
mv "$NEXT_BACKUP" "$BACKUP"
trap - EXIT
echo "prepared ${target:0:12}"
