#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Linux || $(uname -m) != aarch64 || $(id -u) == 0 ]]; then
  echo 'Requires an unprivileged ARM64 Ubuntu Linux VM' >&2
  exit 1
fi
umask 077
source_dir="$HOME/peri-sandbox-source"
runtime=/opt/peri-runtime
node_version=24.22.1
bun_version=1.4.3
peri_hash=323a8e491a26e71be93a4e129ef60b18c7a53098b7bf252a5f309affe3af36c1
cd "$source_dir"
staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT

sudo -n apt-get update
sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y bubblewrap curl ca-certificates ripgrep socat build-essential xz-utils strace
for path in "$runtime" /srv/peri /srv/peri/sandboxes; do
  if [[ -L "$path" ]]; then echo "Refusing symlinked acceptance path: $path" >&2; exit 1; fi
done
sudo -n install -d -m 0755 -o "$(id -u)" -g "$(id -g)" "$runtime" "$runtime/bin"
if [[ ! -x "$runtime/node/bin/node" || $("$runtime/node/bin/node" --version) != "v$node_version" ]]; then
  curl -fLS --retry 3 "https://nodejs.org/dist/v${node_version}/node-v${node_version}-linux-arm64.tar.xz" -o "$staging/node.tar.xz"
  curl -fLS --retry 3 "https://nodejs.org/dist/v${node_version}/SHASUMS256.txt" -o "$staging/SHASUMS256.txt"
  (cd "$staging" && grep "  node-v${node_version}-linux-arm64.tar.xz$" SHASUMS256.txt | sed 's@node-v.*\.tar\.xz@node.tar.xz@' | sha256sum -c -)
  tar -xJf "$staging/node.tar.xz" -C "$staging"
  rm -rf -- "$runtime/node"
  mv "$staging/node-v${node_version}-linux-arm64" "$runtime/node"
fi
ln -sfn "$runtime/node/bin/node" "$runtime/bin/node"
ln -sfn "$runtime/node/bin/npm" "$runtime/bin/npm"
export PATH="$runtime/bin:$runtime/node/bin:/usr/bin:/bin"

if [[ ! -x "$runtime/bin/bun" || $("$runtime/bin/bun" --version) != "$bun_version" ]]; then
  npm install --global --prefix "$runtime" "bun@${bun_version}"
fi
test "$(bun --version)" = "$bun_version"

if [[ ! -x "$runtime/bin/peri" ]]; then
  curl -fLS --retry 3 -H 'Accept: application/octet-stream' \
    'https://api.github.com/repos/KonghaYao/peri/releases/assets/674268052?download=1' -o "$staging/peri.tar.gz"
  printf '%s  %s\n' 750db3c41a3eb7d2b65d6ac8b4aedae338a72025cfdd747ef0c43368e9a98a6d "$staging/peri.tar.gz" | sha256sum -c -
  tar -xzf "$staging/peri.tar.gz" -C "$runtime/bin"
fi
printf '%s  %s\n' "$peri_hash" "$runtime/bin/peri" | sha256sum -c -
bun install --frozen-lockfile
bun run postinstall
bun run build
bun run test
sudo -n install -d -m 0700 -o "$(id -u)" -g "$(id -g)" /srv/peri /srv/peri/sandboxes
if [[ -L /srv/peri/settings.json ]]; then
  echo 'Refusing symlinked acceptance settings' >&2
  exit 1
fi
if [[ ! -f /srv/peri/settings.json ]]; then
  install -m 0600 examples/settings.acceptance.json /srv/peri/settings.json
fi
node -e '
const fs = require("node:fs");
const crypto = require("node:crypto");
const file = "/srv/peri/settings.json";
const settings = JSON.parse(fs.readFileSync(file, "utf8"));
settings.periPath = "/opt/peri-runtime/bin/peri";
settings.periSha256 = crypto.createHash("sha256").update(fs.readFileSync(settings.periPath)).digest("hex");
fs.writeFileSync(file, JSON.stringify(settings, null, 2) + "\n", { mode: 0o600 });
'
chmod 0600 /srv/peri/settings.json
export XDG_RUNTIME_DIR="/run/user/$(id -u)"
export DBUS_SESSION_BUS_ADDRESS="unix:path=$XDG_RUNTIME_DIR/bus"
node dist/src/cli.js doctor --settings /srv/peri/settings.json
node dist/tests/integration/linux.js /srv/peri/settings.json
node dist/tests/integration/resources.js /srv/peri/settings.json
