#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
  echo 'Requires an Apple Silicon macOS host with OrbStack (single-instance Linux acceptance)' >&2
  exit 1
fi
for executable in orb orbctl jq; do command -v "$executable" >/dev/null || { echo "Missing $executable" >&2; exit 1; }; done
machine=${PERI_ACCEPTANCE_VM:-peri-sandbox-accept}
if [[ ! $machine =~ ^[a-zA-Z0-9][a-zA-Z0-9-]*$ ]]; then
  echo 'Invalid VM name' >&2
  exit 1
fi
repo=$(cd "$(dirname "$0")/.." && pwd)
machines=$(orbctl list --format json)
if ! jq -e --arg machine "$machine" 'any(.[]; .name == $machine)' >/dev/null <<< "$machines"; then
  orbctl create --isolated --memory 4G --cpus 2 --disk 24G ubuntu:24.04 "$machine"
fi
if ! orbctl info "$machine" --format json | jq -e --arg machine "$machine" '
  .record.name == $machine and .record.config.isolated == true and
  .record.config.forward_ssh_agent == false and
  .record.image.distro == "ubuntu" and .record.image.version == "noble" and
  .record.image.arch == "arm64"
' >/dev/null; then
  echo "Refusing to provision $machine: expected an isolated Ubuntu 24.04 ARM64 VM without SSH agent forwarding" >&2
  exit 1
fi
archive=$(mktemp /tmp/peri-sandbox-source.XXXXXX)
trap 'rm -f "$archive"' EXIT
tar -czf "$archive" -C "$repo" package.json bun.lock tsconfig.json src tests examples scripts README.md COMPATIBILITY.md peri-sandbox-design.md
chmod 0644 "$archive"
orbctl push -m "$machine" "$archive" peri-sandbox-source.tar.gz
orb -m "$machine" -u root sh -c 'archive="/home/$1/peri-sandbox-source.tar.gz"; test -f "$archive" && test ! -L "$archive" && chmod 0644 "$archive"' sh "$(id -un)"
orb -m "$machine" bash -lc '
  set -euo pipefail
  source_dir="$HOME/peri-sandbox-source"
  archive="$HOME/peri-sandbox-source.tar.gz"
  staging=$(mktemp -d "$HOME/peri-sandbox-source.XXXXXX")
  trap '\''rm -rf "$staging"; rm -f "$archive"'\'' EXIT
  if [[ -e "$source_dir" || -L "$source_dir" ]]; then
    if [[ ! -d "$source_dir" || -L "$source_dir" || ! -f "$source_dir/.peri-acceptance-source" ]]; then
      echo "Refusing to replace unmarked source directory: $source_dir" >&2
      exit 1
    fi
  fi
  tar -xzf "$archive" -C "$staging"
  touch "$staging/.peri-acceptance-source"
  if [[ -d "$source_dir" ]]; then rm -rf -- "$source_dir"; fi
  mv -- "$staging" "$source_dir"
  bash "$source_dir/scripts/provision-acceptance-vm.sh"
'
