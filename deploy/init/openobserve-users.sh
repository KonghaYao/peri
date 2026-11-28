#!/bin/sh
# Create the two dedicated OpenObserve accounts used by peri-swarm.
#
# OpenObserve OSS v1.2.0 only accepts admin/service_account through the user
# API; viewer and editor are rejected. Both accounts are therefore admin. This
# is not a read-only boundary: the dashboard account can write, and the
# dashboard's SELECT-only check is client-side validation only.
#
# Idempotent: an existing account with the expected admin role is accepted.
# Never prints passwords. Exits non-zero on real failures.

set -eu
umask 077

OPENOBSERVE_URL="${OPENOBSERVE_URL:-http://openobserve:5080}"
OPENOBSERVE_ORG="${OPENOBSERVE_ORG:-default}"

: "${OPENOBSERVE_ROOT_EMAIL:?OPENOBSERVE_ROOT_EMAIL is required}"
: "${OPENOBSERVE_ROOT_PASSWORD:?OPENOBSERVE_ROOT_PASSWORD is required}"
: "${OPENOBSERVE_INGEST_EMAIL:?OPENOBSERVE_INGEST_EMAIL is required}"
: "${OPENOBSERVE_INGEST_PASSWORD:?OPENOBSERVE_INGEST_PASSWORD is required}"
: "${OPENOBSERVE_DASHBOARD_EMAIL:?OPENOBSERVE_DASHBOARD_EMAIL is required}"
: "${OPENOBSERVE_DASHBOARD_PASSWORD:?OPENOBSERVE_DASHBOARD_PASSWORD is required}"

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT HUP INT TERM

log() {
    printf '%s\n' "$*"
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# Escape backslash and double quote. Credentials are expected to be single-line.
json_escape() {
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# Wait for the HTTP server before touching the user API.
attempt=0
max_attempts="${OPENOBSERVE_WAIT_ATTEMPTS:-120}"
health_status=""
while [ "$attempt" -lt "$max_attempts" ]; do
    health_status="$(curl -sS -o /dev/null -w '%{http_code}' \
        --connect-timeout 3 --max-time 5 \
        "$OPENOBSERVE_URL/healthz" 2>/dev/null || true)"
    if [ "$health_status" = "200" ]; then
        break
    fi
    attempt=$((attempt + 1))
    sleep 1
done
if [ "$health_status" != "200" ]; then
    fail "openobserve healthz not ready (last status ${health_status:-none})"
fi
log "openobserve: ready"

# True when the email belongs to a user whose role is admin.
has_admin_user() {
    email="$1"
    users_file="$WORK_DIR/users.json"
    status="$(curl -sS -o "$users_file" -w '%{http_code}' \
        --connect-timeout 3 --max-time 10 \
        -u "$OPENOBSERVE_ROOT_EMAIL:$OPENOBSERVE_ROOT_PASSWORD" \
        "$OPENOBSERVE_URL/api/$OPENOBSERVE_ORG/users" 2>/dev/null || true)"
    if [ "$status" != "200" ]; then
        fail "openobserve user list failed (status ${status:-none})"
    fi
    # The response is a flat user list. Split objects and require the email and
    # role in the same object. grep -F avoids treating the email as a regex.
    tr '}' '\n' < "$users_file" \
        | grep -F "\"email\":\"$email\"" \
        | grep -F '"role":"admin"' >/dev/null 2>&1
}

create_user() {
    email="$1"
    password="$2"
    label="$3"
    body_file="$WORK_DIR/create.json"
    response_file="$WORK_DIR/create-response.json"

    {
        printf '{"email":"'
        json_escape "$email"
        printf '","password":"'
        json_escape "$password"
        printf '","role":"admin"}'
    } > "$body_file"

    status="$(curl -sS -o "$response_file" -w '%{http_code}' \
        --connect-timeout 3 --max-time 15 \
        -u "$OPENOBSERVE_ROOT_EMAIL:$OPENOBSERVE_ROOT_PASSWORD" \
        -H 'Content-Type: application/json' \
        --data-binary "@$body_file" \
        "$OPENOBSERVE_URL/api/$OPENOBSERVE_ORG/users" 2>/dev/null || true)"

    case "$status" in
        200)
            log "$label: created"
            return 0
            ;;
        400|409)
            # Duplicate creation is the normal idempotent path. Accept it only
            # when the existing account is actually an admin.
            if grep -F '"message":"User already exists"' "$response_file" >/dev/null 2>&1; then
                if has_admin_user "$email"; then
                    log "$label: already exists"
                    return 0
                fi
                fail "$label: exists with an unexpected role"
            fi
            fail "$label: create failed (status $status)"
            ;;
        *)
            fail "$label: create failed (status ${status:-none})"
            ;;
    esac
}

create_user "$OPENOBSERVE_INGEST_EMAIL" "$OPENOBSERVE_INGEST_PASSWORD" "peri-swarm-ingest"
create_user "$OPENOBSERVE_DASHBOARD_EMAIL" "$OPENOBSERVE_DASHBOARD_PASSWORD" "peri-swarm-dashboard"
log "openobserve-users: ready"
