# Client default spaces and tenant selection

`harness setup` and `harness connect` discover the authorized server catalog with
one `GET /v1/namespaces`. They never create spaces or grant permissions. Credentials
remain in the configured token environment variable.

```sh
harness --config /path/to/client.json --url https://harness.example setup
```

When both input and diagnostic output are terminals and there is no explicit or
existing local namespace, the client displays each space's name, ID, status, read
right and write right. Choose the default write space by number or ID. An optional
second prompt accepts up to eight distinct readable IDs, comma separated; Enter
leaves the read defaults to the server for the selected space. `q` or end of input
cancels without replacing the configuration.

Existing authorized local selections and project mappings are preserved.
Explicit `--namespace` and `--read-namespaces` override them. Setup validates that
the write choice is writable and every read choice is readable before saving.
Successful setup merges the configuration under a lock, backs up an existing
file, and replaces it atomically with owner-only permissions on Unix.

## Bots and first business calls

Piped input never triggers a selection prompt. With exactly one authorized write
space, setup/connect selects it and reports
`"namespace_selection": "single_authorized_write"`. With zero or multiple writable
spaces, it fails with a `namespace_choice_required` JSON error on stderr containing
the authorized catalog and an actionable hint. A server write default does not
resolve an ambiguous local selection.

```sh
harness --config /path/to/client.json --namespace ns-device \
  --read-namespaces ns-device,shared connect
```

The first business call without an explicit or saved namespace uses the same
one-time discovery and selection flow. A successful selection is saved before
continuing the original request. Multiple writable spaces in a non-interactive
call return the catalog and hint instead of sending the business request. Once a
default is saved, subsequent calls add no discovery requests. The first successful
read or scoped-v2 write therefore normally uses two requests; later calls use one.
Legacy single-shared credentials can select `shared` automatically without
inventing an Agent identity. On an old server without catalog discovery, pass an
explicit `--namespace shared` for a legacy single-space deployment.

Offline instructions/identity and health, stats, namespaces and doctor skip this
gate. Explicit namespace choices also skip it. Setup/connect always revalidate
selections against the catalog.

Daily business commands return `{"mode":"SKIP"}` locally, before credentials,
space discovery, payload reads or HTTP. This also covers `route`/`start` and
log-only `finish` when a local project binding resolves to `daily`. No default
selection is persisted, and no server metrics event is created.

Tracked `finish` and `abort` retain server resolution of the original Session's
namespace, including historical shared Sessions. Stdin payloads, SHA conflict
checks, idempotency keys, v1 write preflight and negotiated scoped-v2 writes retain
their existing behavior.

## Tenant selector

Select a gateway tenant with global `--tenant`, `HARNESS_TENANT`, or config
`"tenant"`, in that precedence order. It must be a lowercase slug of 1–128 ASCII
characters: an initial letter or digit, followed by letters, digits or hyphens.

```sh
harness --config /path/to/team-red.json --tenant team-red \
  --url https://harness.example setup
```

The selector is sent as `X-Harness-Tenant` on every HTTP request, including
discovery, doctor, health and v1 preflight. It selects the gateway destination;
credentials still determine authorization. The URL remains an origin with no
tenant path, query, credentials or fragment. The gateway must dispatch this header;
the client does not implement gateway routing or tenant authorization.

Every successful tenant response must acknowledge the selector with an exact
`X-Harness-Tenant` response header. The client adds verified `tenant` provenance to
business JSON and to `identity.tenant` when an identity object exists. An old
server that ignores the selector cannot produce a successful verified result.

Tenant business writes use the gateway's guarded prefix:
`/v3/tenant/v2/scoped/start`, for example, or `/v3/tenant/v1/documents` for a v1
write. The gateway validates the selector and removes `/v3/tenant` before
dispatching to its child store. An old single-tenant server returns 404 before
executing the write. Read-only POSTs (`route`, `eval-routing`, `check-template`)
retain their v1 paths. Existing v1 write preflight still checks health and its
tenant acknowledgement; scoped-v2 needs no additional probe or capability cache.

Setup/connect/init persist the selector. Identity, offline instructions JSON and
doctor report it; full text instructions include it when configured. These local
reports do not verify authorization. Doctor remains read-only.

Use a separate configuration file for each tenant. A selector that differs from a
saved tenant is rejected before HTTP or config replacement. Adding a tenant to a
legacy config that already has an identity, namespace, read scopes, write protocol
or project bindings is also rejected: initialize a separate config rather than
implicitly reusing those selections. A fresh config, or a connection-only config
containing URL/token environment name, can be connected to a tenant.
Changing a tenant configuration's origin clears the inherited Agent, write
namespace, read namespace and write protocol choices for that invocation.
Setup/connect or first business use must select defaults from the new origin;
explicit namespace/read choices can also be supplied. Local project mappings and
the token environment variable name remain available. A successful configuration
save backs up the previous file.

Omitting the selector preserves single-tenant behavior and sends no tenant header.
Offline instructions continue to work without credentials or network access.
