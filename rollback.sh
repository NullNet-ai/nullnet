#!/usr/bin/env bash

set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=/opt/nullnet
BACKUP=$ROOT/previous
STATE=$ROOT/state

[[ $# -eq 0 ]] || { echo "rollback.sh takes no arguments" >&2; exit 2; }
[[ $EUID -eq 0 ]] || { echo "Run as root" >&2; exit 1; }
[[ -f $BACKUP/commit && -f $BACKUP/target && -f $BACKUP/components ]] || {
  echo "No previous deployment is available" >&2
  exit 1
}

if [[ ${NULLNET_ROLLBACK_ACTIVE:-} != 1 ]]; then
  systemd-run --quiet --unit=nullnet-rollback --collect \
    --setenv=NULLNET_ROLLBACK_ACTIVE=1 "$REPO/rollback.sh"
  echo "Rollback started; it will continue if SSH disconnects"
  echo "Status: journalctl -u nullnet-rollback --no-pager"
  exit 0
fi

exec 9>"$ROOT/deploy.lock"
flock -n 9 || { echo "Another deployment command is running" >&2; exit 1; }

commit=$(<"$BACKUP/commit")
target=$(<"$BACKUP/target")
mapfile -t components <"$BACKUP/components"
((${#components[@]})) || { echo "Previous deployment has no components" >&2; exit 1; }
git -C "$REPO" cat-file -e "$commit^{commit}"
git -C "$REPO" cat-file -e "$target^{commit}"

already_rolled_back=true
[[ $(git -C "$REPO" rev-parse 'HEAD^{commit}') == "$commit" ]] || already_rolled_back=false
declare -a units=() first_units=()
restart_client=false
for component in "${components[@]}"; do
  case "$component" in
    server | proxy | client) ;;
    *) echo "Invalid backup component: $component" >&2; exit 1 ;;
  esac
  units+=("nullnet-$component.service")
  if [[ $component == client ]]; then
    restart_client=true
  else
    first_units+=("nullnet-$component.service")
  fi
  binary="$BACKUP/bin/nullnet-$component"
  live_binary="$REPO/target/release/nullnet-$component"
  if [[ ! -x $binary || ! -x $live_binary ]] || ! cmp -s "$binary" "$live_binary" ||
    [[ ! -f $STATE/$component ]] ||
    [[ $(<"$STATE/$component") != "$commit" ]] ||
    ! systemctl is-active --quiet "nullnet-$component.service"; then
    already_rolled_back=false
  fi
done
if $already_rolled_back; then
  echo "already rolled back to ${commit:0:12}"
  exit 0
fi

echo "Restoring ${commit:0:12}: ${components[*]}"

for component in "${components[@]}"; do
  binary="$BACKUP/bin/nullnet-$component"
  [[ -x $binary ]] || { echo "Missing backup: $binary" >&2; exit 1; }
done

database_required=false
if [[ " ${components[*]} " == *" server "* ]] &&
  ! git -C "$REPO" diff --quiet "$commit" "$target" -- \
    members/nullnet-server/src/db/migrations; then
  database_required=true
fi
if $database_required || [[ -f $BACKUP/database || -f $BACKUP/database-path ]]; then
  [[ -f $BACKUP/database && -f $BACKUP/database-path ]] || {
    echo "Previous database backup is incomplete" >&2
    exit 1
  }
  database=$(<"$BACKUP/database-path")
fi

git -C "$REPO" switch --detach "$commit"
for component in "${components[@]}"; do
  install -m 755 "$BACKUP/bin/nullnet-$component" "$REPO/target/release/nullnet-$component"
done

if [[ -n ${database:-} ]]; then
  echo "Restoring database snapshot"
  systemctl stop "${units[@]}"
  install -m 600 "$BACKUP/database" "$database"
  rm -f "$database-wal" "$database-shm"
  action=start
else
  action=restart
fi

if ((${#first_units[@]})); then
  echo "${action^}: ${first_units[*]}"
  systemctl "$action" "${first_units[@]}"
  sleep 3
  for unit in "${first_units[@]}"; do
    systemctl is-active --quiet "$unit"
  done
fi
if $restart_client; then
  echo "${action^}: nullnet-client.service"
  systemctl "$action" nullnet-client.service
  sleep 3
  systemctl is-active --quiet nullnet-client.service
fi
mkdir -p "$STATE"
for component in "${components[@]}"; do
  printf '%s\n' "$commit" >"$STATE/$component"
done
echo "rolled back to ${commit:0:12}"
