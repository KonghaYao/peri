#!/usr/bin/env python3
"""Provision the Swarm state database on a running Loom dev stack.

S6c2 (`design §4.1`): the Peri session store and the Swarm state store are two
independent logical databases. `dev.py` seeds only the session database
(`local_default/demo/main`); the Swarm database (`local_default/swarm/main`) is
created here through the platform management API, before the test runs.

Usage::

    python3 peri-swarm/scripts/provision_state_db.py \\
        --connection-file /tmp/peri-swarm-s6c2/run/connection.json

    # machine-readable public fields (still no credentials)
    python3 peri-swarm/scripts/provision_state_db.py \\
        --connection-file ... --json

The helper reads `connection.json`, which holds:

- `endpoint` — the management/Hrana origin (`http://127.0.0.1:9497`);
- `project_api_key` — the project credential. It is used only in the
  `Authorization` header. It is never printed, logged, or written to disk;
- `database` — the seeded session database (`namespace` + `branch`), used as
  the default tenant/branch for the state database;
- `ca_file` — present when the stack runs with `--tls`.

The create call is idempotent **for the same `request_id`**: the platform
manager treats an existing database with the same project and operation id as a
successful retry (it only reconciles the placement). A different request id for
an already reserved identity is an explicit API error, which this helper reports
instead of retrying or reusing another database. After the management reply the
helper probes Hrana (`POST /db/<tenant>/<database>/<branch>/v3/pipeline` with
`SELECT 1`) so a registered-but-unreachable database fails here, not later in
the test.

Exit codes: 0 provisioned (created or already present) and Hrana reachable,
1 input/connection failure, 2 management rejection, 3 Hrana probe failure.
"""

import argparse
import json
import secrets
import ssl
import sys
import urllib.error
import urllib.request

DEFAULT_PROJECT = "default"
DEFAULT_DATABASE = "swarm"
DEFAULT_BRANCH = "main"
DEFAULT_REQUEST_ID = "swarm-state-db"
DEFAULT_ROLE = "writer"
HRANA_TIMEOUT_SECONDS = 10


class Failure(Exception):
    """An operator-visible failure with a stable exit code."""

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


def public_endpoint(endpoint):
    return endpoint.rstrip("/")


def parse_connection(path):
    try:
        with open(path, "r", encoding="utf-8") as handle:
            connection = json.load(handle)
    except OSError as error:
        raise Failure(1, "cannot read connection file {}: {}".format(path, error)) from error
    except ValueError as error:
        raise Failure(1, "connection file {} is not JSON: {}".format(path, error)) from error
    for name in ("endpoint", "project_api_key", "database"):
        if name not in connection:
            raise Failure(1, "connection file {} has no {!r}".format(path, name))
    database = connection["database"] or {}
    namespace = database.get("namespace") or {}
    for name in ("tenant", "database"):
        if not namespace.get(name):
            raise Failure(1, "connection file {} has no database.namespace.{}".format(path, name))
    if not database.get("branch"):
        raise Failure(1, "connection file {} has no database.branch".format(path))
    return connection


def ssl_context(connection):
    endpoint = connection["endpoint"]
    ca_file = connection.get("ca_file")
    if endpoint.startswith("https:"):
        return ssl.create_default_context(cafile=ca_file)
    return None


def management_request(connection, path, body, request_id, timeout=30):
    """POST a management request; return (status, decoded envelope).

    The credential stays inside the header. Transport errors are re-raised with
    the URL only (urllib error text never includes request headers).
    """
    endpoint = public_endpoint(connection["endpoint"])
    payload = dict(body)
    payload["request_id"] = request_id
    headers = {
        "Authorization": "Bearer " + connection["project_api_key"],
        "Content-Type": "application/json",
        "X-Request-ID": request_id,
    }
    request = urllib.request.Request(
        endpoint + path,
        data=json.dumps(payload).encode("utf-8"),
        headers=headers,
        method="POST",
    )
    handlers = [urllib.request.ProxyHandler({}), NoRedirect()]
    context = ssl_context(connection)
    if context is not None:
        handlers.append(urllib.request.HTTPSHandler(context=context))
    client = urllib.request.build_opener(*handlers)
    try:
        try:
            incoming = client.open(request, timeout=timeout)
        except urllib.error.HTTPError as failure:
            if not 400 <= failure.code <= 599:
                raise
            incoming = failure
        with incoming:
            encoded = incoming.read(4 * 1024 * 1024 + 1)
    except (urllib.error.URLError, OSError) as error:
        raise Failure(1, "{} {} failed: {}".format(endpoint, path, error)) from error
    try:
        envelope = json.loads(encoded.decode("utf-8"))
    except ValueError as error:
        raise Failure(1, "{} {} returned non-JSON: {}".format(endpoint, path, error)) from error
    return incoming.status if hasattr(incoming, "status") else incoming.code, envelope


class NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect would hide which endpoint answered; refuse it."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: D102
        return None


def database_status(connection, database):
    """Probe whether the target database was already registered.

    Informational only: the create call below is the authoritative, idempotent
    operation. A missing database answers with an error envelope.
    """
    try:
        status, envelope = management_request(
            connection,
            "/v1/databases/status",
            {"database": database},
            secrets.token_hex(16),
        )
    except Failure:
        return None
    if status == 200 and envelope.get("ok"):
        return envelope.get("result")
    return None


def create_database(connection, project, tenant, database, branch, request_id):
    body = {
        "request_id": request_id,
        "project": project,
        "database": {
            "namespace": {"tenant": tenant, "database": database},
            "branch": branch,
        },
        "role": DEFAULT_ROLE,
    }
    status, envelope = management_request(
        connection, "/v1/databases/create", body, request_id
    )
    if status != 200 or not envelope.get("ok"):
        failure = envelope.get("error") or {}
        raise Failure(
            2,
            "management rejected /v1/databases/create (HTTP {}, code={}): {}".format(
                status,
                failure.get("code", "unknown"),
                json.dumps(failure, sort_keys=True),
            ),
        )
    return envelope.get("result") or {}


def hrana_probe(connection, tenant, database, branch):
    """Run `SELECT 1` through the Hrana v3 pipeline on the target database."""
    endpoint = public_endpoint(connection["endpoint"])
    url = "{}/db/{}/{}/{}/v3/pipeline".format(endpoint, tenant, database, branch)
    body = {
        "baton": None,
        "requests": [
            {
                "type": "execute",
                "stmt": {"sql": "SELECT 1 AS ok", "args": [], "named_args": [], "want_rows": True},
            },
            {"type": "close"},
        ],
    }
    request = urllib.request.Request(
        url,
        data=json.dumps(body).encode("utf-8"),
        headers={
            "Authorization": "Bearer " + connection["project_api_key"],
            "Content-Type": "application/json",
        },
        method="POST",
    )
    handlers = [urllib.request.ProxyHandler({}), NoRedirect()]
    context = ssl_context(connection)
    if context is not None:
        handlers.append(urllib.request.HTTPSHandler(context=context))
    client = urllib.request.build_opener(*handlers)
    try:
        try:
            incoming = client.open(request, timeout=HRANA_TIMEOUT_SECONDS)
        except urllib.error.HTTPError as failure:
            incoming = failure
        with incoming:
            encoded = incoming.read(1024 * 1024 + 1)
        status = incoming.status if hasattr(incoming, "status") else incoming.code
    except (urllib.error.URLError, OSError) as error:
        raise Failure(3, "Hrana probe {} failed: {}".format(url, error)) from error
    if status != 200:
        raise Failure(
            3,
            "Hrana probe {} returned HTTP {}: {}".format(
                url, status, encoded[:400].decode("utf-8", "replace")
            ),
        )
    try:
        response = json.loads(encoded.decode("utf-8"))
    except ValueError as error:
        raise Failure(3, "Hrana probe {} returned non-JSON: {}".format(url, error)) from error
    results = response.get("results") or []
    if not results or results[0].get("type") != "ok":
        raise Failure(
            3,
            "Hrana probe {} returned no execute result: {}".format(
                url, json.dumps(response)[:400]
            ),
        )
    result = (results[0].get("response") or {}).get("result") or {}
    rows = result.get("rows") or []
    if not rows:
        raise Failure(3, "Hrana probe {} returned no rows for SELECT 1".format(url))
    value = rows[0][0] if rows[0] else None
    if not isinstance(value, dict) or str(value.get("value")) != "1":
        raise Failure(
            3, "Hrana probe {} did not answer SELECT 1=1: {}".format(url, json.dumps(rows))
        )
    return url


def main(argv):
    parser = argparse.ArgumentParser(
        description="Create the Swarm state database on a running Loom dev stack."
    )
    parser.add_argument("--connection-file", required=True, help="dev.py connection.json")
    parser.add_argument("--project", default=DEFAULT_PROJECT)
    parser.add_argument("--database", default=DEFAULT_DATABASE)
    parser.add_argument("--tenant", default=None, help="defaults to the connection tenant")
    parser.add_argument("--branch", default=None, help="defaults to the connection branch")
    parser.add_argument("--request-id", default=DEFAULT_REQUEST_ID)
    parser.add_argument("--skip-hrana", action="store_true", help="provision without probing Hrana")
    parser.add_argument("--json", action="store_true", help="print public fields as one JSON object")
    arguments = parser.parse_args(argv)

    connection = parse_connection(arguments.connection_file)
    seeded = connection["database"]
    tenant = arguments.tenant or seeded["namespace"]["tenant"]
    branch = arguments.branch or seeded["branch"]
    target = {"namespace": {"tenant": tenant, "database": arguments.database}, "branch": branch}

    existed_before = database_status(connection, target) is not None
    placement = create_database(
        connection, arguments.project, tenant, arguments.database, branch, arguments.request_id
    )
    hrana_url = None
    if not arguments.skip_hrana:
        hrana_url = hrana_probe(connection, tenant, arguments.database, branch)

    endpoint = public_endpoint(connection["endpoint"])
    public = {
        "endpoint": endpoint,
        "project": arguments.project,
        "tenant": tenant,
        "database": arguments.database,
        "branch": branch,
        "role": placement.get("role", DEFAULT_ROLE),
        "desired_state": placement.get("desired_state"),
        "phase": placement.get("phase"),
        "epoch": placement.get("epoch"),
        "request_id": arguments.request_id,
        "existed_before": existed_before,
        "hrana_url": hrana_url,
        "session_locator": "{}/db/{}/{}/{}/".format(
            endpoint, tenant, seeded["namespace"]["database"], seeded["branch"]
        ),
        "state_locator": "{}/db/{}/{}/{}/".format(endpoint, tenant, arguments.database, branch),
    }
    if arguments.json:
        print(json.dumps(public, sort_keys=True))
    else:
        print(
            "state database {}: {} (already registered before this run: {})".format(
                "present" if existed_before else "created",
                public["state_locator"],
                "yes" if existed_before else "no",
            )
        )
        print(
            "placement: role={} desired_state={} phase={} epoch={}".format(
                public["role"],
                public["desired_state"],
                public["phase"],
                public["epoch"],
            )
        )
        print(
            "hrana: {}".format(
                "ok via " + hrana_url if hrana_url else "skipped (--skip-hrana)"
            )
        )
        print("export PERI_SWARM_SESSION_STORE={}".format(public["session_locator"]))
        print("export PERI_SWARM_STATE_STORE={}".format(public["state_locator"]))
        print("export PERI_SWARM_STORE_TOKEN=<connection.json project_api_key>")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Failure as failure:
        print("provision_state_db: {}".format(failure), file=sys.stderr)
        sys.exit(failure.code)
