#!/usr/bin/env bash

set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=/opt/nullnet
BACKUP=$ROOT/previous
STATE=$ROOT/state

[[ $# -eq 0 ]] || { echo "deploy.sh takes no arguments" >&2; exit 2; }
[[ $EUID -eq 0 ]] || { echo "Run as root" >&2; exit 1; }
[[ -f $BACKUP/commit && -f $BACKUP/target && -f $BACKUP/components ]] || {
  echo "Run prepare.sh first" >&2
  exit 1
}

if [[ ${NULLNET_DEPLOY_ACTIVE:-} != 1 ]]; then
  systemd-run --quiet --unit=nullnet-deploy --collect \
    --setenv=NULLNET_DEPLOY_ACTIVE=1 "$REPO/deploy.sh"
  echo "Deployment started; it will continue if SSH disconnects"
  echo "Status: journalctl -u nullnet-deploy --no-pager"
  exit 0
fi

exec 9>"$ROOT/deploy.lock"
flock -n 9 || { echo "Another deployment command is running" >&2; exit 1; }

target=$(<"$BACKUP/target")
commit=$(<"$BACKUP/commit")
mapfile -t components <"$BACKUP/components"
((${#components[@]})) || { echo "Prepared deployment has no components" >&2; exit 1; }
git -C "$REPO" cat-file -e "$commit^{commit}"
git -C "$REPO" cat-file -e "$target^{commit}"
[[ $(git -C "$REPO" rev-parse 'HEAD^{commit}') == "$target" ]] || {
  echo "Repository is not at the prepared commit; run prepare.sh again" >&2
  exit 1
}
declare -a first_units=()
restart_client=false

already_active=true
for component in "${components[@]}"; do
  case "$component" in
    server | proxy | client) ;;
    *) echo "Invalid prepared component: $component" >&2; exit 1 ;;
  esac
  binary="$REPO/target/release/nullnet-$component"
  hash_file="$BACKUP/prepared-$component.sha256"
  [[ -x $binary && -f $hash_file && -x $BACKUP/bin/nullnet-$component ]] &&
    [[ $(sha256sum "$binary" | cut -d' ' -f1) == $(<"$hash_file") ]] || {
      echo "Prepared binary is missing or changed: nullnet-$component" >&2
      exit 1
    }
  if [[ $component == client ]]; then
    restart_client=true
  else
    first_units+=("nullnet-$component.service")
  fi
  if [[ ! -f $STATE/$component ]] ||
    [[ $(<"$STATE/$component") != "$target" ]] ||
    ! systemctl is-active --quiet "nullnet-$component.service"; then
    already_active=false
  fi
done
if [[ " ${components[*]} " == *" server "* ]] &&
  ! git -C "$REPO" diff --quiet "$commit" "$target" -- \
    members/nullnet-server/src/db/migrations; then
  [[ -f $BACKUP/database && -f $BACKUP/database-path ]] || {
    echo "Prepared database backup is missing" >&2
    exit 1
  }
fi
if $already_active; then
  echo "already active at ${target:0:12}"
  exit 0
fi

if ((${#first_units[@]})); then
  echo "Restarting: ${first_units[*]}"
  systemctl restart "${first_units[@]}"
  sleep 3
  for unit in "${first_units[@]}"; do
    systemctl is-active --quiet "$unit"
  done
fi
if $restart_client; then
  echo "Restarting: nullnet-client.service"
  systemctl restart nullnet-client.service
  sleep 3
  systemctl is-active --quiet nullnet-client.service
fi
mkdir -p "$STATE"
for component in "${components[@]}"; do
  printf '%s\n' "$target" >"$STATE/$component"
done
echo "active at ${target:0:12}"
