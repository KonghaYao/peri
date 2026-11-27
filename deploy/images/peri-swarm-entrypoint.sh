#!/bin/sh
# Entrypoint for the peri-swarm container.
#
# Compose hands the model settings over as a secret, but the gateway reads the
# same document from $HOME/.peri/settings.json. Copy it there (never echo it),
# restore the stable machine id, then exec the gateway so tini keeps PID 1 and
# signals reach the process directly.
#
# Replicas of one group share this HOME (the same named volume), and the design
# requires them to share PERI_SWARM_MACHINE_ID. Both files below are therefore
# published atomically so a concurrent first start cannot observe a partial
# document or two different machine ids.
set -eu

SETTINGS_SECRET="${PERI_SWARM_SETTINGS_SECRET:-/run/secrets/swarm_settings}"
SETTINGS_PATH="${HOME:?HOME must be set}/.peri/settings.json"

if [ ! -r "$SETTINGS_SECRET" ]; then
    echo "peri-swarm-entrypoint: settings secret not readable at $SETTINGS_SECRET" >&2
    exit 1
fi

mkdir -p "$(dirname "$SETTINGS_PATH")"
umask 077
SETTINGS_TMP="$SETTINGS_PATH.tmp.$$"
cp "$SETTINGS_SECRET" "$SETTINGS_TMP"
chmod 600 "$SETTINGS_TMP"
mv "$SETTINGS_TMP" "$SETTINGS_PATH"

# PERI_SWARM_MACHINE_ID must be a UUID that stays the same across restarts and
# is shared by every replica. Noclobber makes the create exclusive on Linux.
MACHINE_ID_FILE="$HOME/machine-id"
if [ ! -s "$MACHINE_ID_FILE" ]; then
    if (set -C; cat /proc/sys/kernel/random/uuid > "$MACHINE_ID_FILE") 2>/dev/null; then
        :
    else
        # Another replica won the create; wait for its write to land.
        tries=0
        while [ ! -s "$MACHINE_ID_FILE" ]; do
            tries=$((tries + 1))
            if [ "$tries" -gt 100 ]; then
                echo "peri-swarm-entrypoint: machine id $MACHINE_ID_FILE stayed empty" >&2
                exit 1
            fi
            sleep 0.1
        done
    fi
fi
MACHINE_ID=$(tr -d '[:space:]' < "$MACHINE_ID_FILE")
case "$MACHINE_ID" in
    ????????-????-????-????-????????????) ;;
    *)
        echo "peri-swarm-entrypoint: invalid machine id in $MACHINE_ID_FILE" >&2
        exit 1
        ;;
esac
export PERI_SWARM_MACHINE_ID="$MACHINE_ID"

mkdir -p "${PERI_SWARM_WORKSPACE_ROOT:?PERI_SWARM_WORKSPACE_ROOT must be set}"

exec /usr/local/bin/peri-swarm
