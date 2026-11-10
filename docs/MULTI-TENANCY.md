# Tenant Gateway

Multi-tenant mode is an explicit startup choice. Each tenant runs its own
authenticated application against its own Store at
`<data-root>/tenants/<tenant-id>`. Tenant selection never grants access.
Single-tenant invocations continue to use the original data directory and
credential settings when `--tenants-config` / `HARNESS_TENANTS_CONFIG` is absent.

## Tenant and namespace

A tenant is the boundary for a complete installation's data and credentials.
Each tenant has separate principals, administrator token, browser sessions,
namespace registry, control audit, documents, shared memory, execution records,
idempotency receipts, metrics, and routing evaluation reports. Tenant applications
do not share Store state, registries, caches, or session state.

A namespace divides authorized memory **inside one tenant**. The namespace named
`shared` is shared only within its owning tenant. Reading another namespace's
authorized shared background never crosses a tenant boundary. Different tenants
can reuse namespace, agent, project, session, workstream, and execution IDs as well
as idempotency keys. A tenant's credential cannot read or write another tenant,
even when those IDs match.

## Explicit configuration

Copy [the tenant configuration example](../examples/tenants.example.json) into a
deployment-specific file. The top-level `tenants` array must contain 1-64 entries.
Unknown fields, invalid IDs, duplicate IDs, missing credentials, and ambiguous
authentication configuration fail startup. IDs use 1-128 lowercase ASCII letters,
digits, or hyphens and must start with a letter or digit.

Each entry supports:

| Field | Meaning |
| --- | --- |
| `id` | Tenant ID and fixed Store directory name. |
| `auth_config` | Existing principals configuration loaded by `auth::load`; relative paths resolve beside the tenant configuration file. |
| `token_env` | Alternative to `auth_config`: one environment variable containing a legacy Agent token with access to this tenant's `shared` namespace. Exactly one authentication option is required. |
| `admin_token_env` | Required environment variable containing this tenant's independent administrator token. |
| `admin_username` | Optional browser login username; requires `admin_password_hash_env`. |
| `admin_password_hash_env` | Environment variable containing the Argon2id PHC hash for this tenant's browser login. |
| `secure_cookie` | JSON boolean, default `true`, for password and token-only sessions alike. It never inherits global administrator settings. Use `false` only for local HTTP development. |

The example's `acme` entry uses
[the existing principals format](../examples/auth.example.json). Those principals
also reference environment variables through `token_env`. Empty grants allow
discovery but do not grant memory access; configure grants through the tenant's
administrator. The example's `personal` entry uses a single Agent token.

Configuration files contain references rather than credentials. Literal token,
password, password hash, and arbitrary data-directory fields are not accepted.
Missing, empty, non-text, or malformed referenced environment values fail closed.
Agent and administrator tokens must be 32-512 bytes without whitespace. The
gateway rejects reuse of any token across tenant or Agent/admin roles, and rejects
identical administrator PHC hashes across tenants. Use independent passwords for
each tenant: differently salted hashes of the same password cannot be recognized
as password reuse without the plaintext.

Create a password hash offline using:

```sh
harness-server hash-password
```

The command prompts twice with hidden terminal input, prints the PHC hash, and
does not open or create a Store. It rejects passwords supplied through arguments or
non-interactive input. Inject the resulting hash and tokens through your service
manager or deployment's secret mechanism. Do not commit them to configuration.

Start the gateway with:

```sh
harness-server --data-dir /srv/harness-multi serve \
  --bind 127.0.0.1:8080 \
  --tenants-config /etc/harness/tenants.json
```

`HARNESS_TENANTS_CONFIG` can supply the configuration path instead of the flag.
Combining it with `--auth-config` or `HARNESS_AUTH_CONFIG` is an error.
Global `HARNESS_TOKEN`, `HARNESS_TOKENS_JSON`, and administrator login variables
may remain in a shared deployment environment, but multi-tenant mode does not
use them or fall back to them. Every tenant's credentials must resolve from its
own configuration. A new tenant requires an explicit configuration change and
restart; this version has no online tenant provisioning or tenant directory API.

## Request selection and authentication

For business requests under `/v1` or `/v2`, and for `/admin/api` including login,
session, and logout, supply exactly one header:

```text
X-Harness-Tenant: acme
```

The header only selects an application. Business requests still need that
tenant's Agent Bearer token and namespace grants. Administrator API calls still
need that tenant's administrator Bearer token or authenticated browser session.
Selecting another tenant cannot carry authority from a different application.
Browser session cookies and login throttling belong to the selected tenant;
restarting the service invalidates browser sessions.

Missing selectors and syntactically valid unknown selectors return the same
generic `404` without listing or echoing tenant IDs. Malformed or repeated headers
return a generic `400`. There is no default tenant. Top-level JSON selector
fields, tenant query parameters, and physical `tenants/<id>/...` document paths
are rejected. Tenant fields inside stored document metadata, templates, or
execution payloads remain ordinary data; they never affect dispatch.

Every response dispatched to a selected application includes
`X-Harness-Tenant: <id>`, including child authentication errors. Gateway errors
before dispatch and common assets do not acknowledge a tenant. Clients should
verify the acknowledgement before using a tenant response.

### Guarded write paths

Tenant-aware clients send business mutations under the exclusive prefix
`/v3/tenant/`, retaining the tenant header and all authentication headers:

```text
POST /v3/tenant/v2/scoped/start
POST /v3/tenant/v2/scoped/finish
PUT  /v3/tenant/v1/documents
```

The gateway strips `/v3/tenant` and forwards the request to that tenant's existing
business router. The prefix accepts business mutation methods only; it cannot
reach health checks, common assets, or administrator APIs. Read operations and
read-only POST operations continue to use their normal paths.

Older single-tenant servers have no guarded route, so these writes return `404`
before reaching a business handler. This prevents an older server from silently
ignoring a new selector header and writing to its original `shared` Store.
The current single-tenant router also rejects a tenant header. No extra preflight
request is needed per write. Direct HTTP integrations may use ordinary `/v1` and
`/v2` business routes with a header against this gateway, but must use guarded
paths when compatibility with older deployments is uncertain.

### Public routes

`GET /healthz` returns only `{"status":"ok"}`. It does not expose tenants,
revisions, documents, credentials, or build information. Check a selected and
authenticated `/v1/health` for that tenant's service health.

`/admin`, `/admin/`, `/admin/console.css`, and `/admin/console.js` serve common,
fixed same-origin assets without authentication or a tenant selector. There is no
external redirect or tenant-specific asset lookup. The browser must select a
tenant before calling protected APIs; the server does not publish a tenant list.
Use HTTPS for deployed browser login. Keep `secure_cookie: true` when HTTPS
terminates at a trusted reverse proxy; explicitly disabling it is for local HTTP.

## Storage, startup, and migration

The gateway validates all configured tenants and credentials before opening
Stores. It locks the multi-tenant data root, opens all tenant Stores in sorted
tenant ID order, and holds their writer locks for the server lifetime. If a lock
or Store fails, startup fails and already acquired locks are released. Separate
Store roots must resolve to distinct directories. Symlinks in the configured
data root, its ancestors, `tenants`, and tenant directories are rejected.

The gateway writes a versioned `.tenancy-mode.json` marker in the data root.
Later single-tenant serve, import, or maintenance commands refuse to open that
marked root as a Store. An invalid marker also fails multi-tenant startup. The
marker can exist after a failed startup: fix the configuration or lock conflict
and retry multi-tenant mode.

A legacy Store with root-level `HEAD`, `revisions`, `.writer.lock`, or `.runtime`
is rejected as a multi-tenant data root. The gateway neither silently ignores
that Store nor auto-imports or migrates it.

To migrate an existing installation:

1. Stop the legacy writer and create or verify its existing Store backup.
2. Choose a separate multi-tenant data root.
3. Restore or copy the complete legacy Store into
   `<new-data-root>/tenants/<chosen-id>` using the existing Store migration
   procedure. Preserve its revisions, receipts, and control records.
4. Configure that tenant's Agent credentials and independent administrator
   credentials, then start the gateway explicitly with the configuration file.
5. Configure clients to send the tenant selector and use guarded write paths,
   then verify authenticated health and data in the intended tenant.

Restoring a Store retains its stored grants and namespace registry. Existing
registry entries remain authoritative; adding a principal in the startup file
does not silently replace persisted grants.

## Backups and offline tools

Existing backup tools operate on **one Store**. For tenant `acme`, point the
backup configuration's `data_dir` at
`/srv/harness-multi/tenants/acme`, not `/srv/harness-multi`. Use a separate backup
configuration, destination prefix, retention policy, and restore target for each
tenant. The parent data root is not a Store and the existing backup tool does
not automatically enumerate or back up all tenants.

Follow [the backup and restore guide](BACKUP-OSS.md) for each selected tenant
root. Knowledge, control records, and receipts follow the existing Store backup
contract; runtime metrics/evaluations remain outside that backup contract.
Secrets and the tenant configuration need their own deployment recovery plan.
Browser sessions are in memory and are not backed up.

Likewise, offline import and maintenance commands must explicitly use the
individual root, for example:

```sh
harness-server --data-dir /srv/harness-multi/tenants/acme maintenance
```

Writer-lock rules are unchanged. Stop the affected service before a command
that needs exclusive Store access. A full data-root filesystem snapshot is an
operator-managed snapshot with its own consistency requirements, not an implicit
multi-tenant feature of the existing per-Store backup tool.
