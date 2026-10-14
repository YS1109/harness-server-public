# Repository instructions

- Read README.md for the current v1 scope. Service and CLI are native Rust; do not invoke the old Python engine or promise full protocol compatibility.
- Markdown is the knowledge source. Keep bounded reads, project/session identity checks, SHA-256 conflict protection, persistent idempotency and atomic multi-document commits.
- This is a single-user, single-writer service for trusted devices. Keep tokens, personal memories, imports and backups out of Git and build contexts.
- Verify changes according to impact. Run affected contract tests for behavior changes; use formatting/static checks for documentation. Do not delegate routine reviews or start independent chats for subtasks.
- Do not switch the user's existing Harness configuration or deploy to a machine without a request. For guided configuration read docs/AI-SETUP.md; default origin is http://localhost:8080. Daily integration instructions are in docs/AGENT-BOOTSTRAP.md; image publishing is in docs/IMAGES.md.

- Build and test locally or in GitHub Actions. Small production hosts should only install built artifacts; do not run Rust/Docker builds on them for validation.
- Namespace scope comes from credential grants and request headers, not user-controlled document paths. Preserve shared legacy receipts and bounded shared-memory inheritance; never inherit another namespace's handoffs or executions.

- Namespace IDs are server-owned. Live grants/defaults and management audit live under control/ in the same atomic store; Agent APIs never expose or mutate control documents. Rank multiple read origins under one shared budget and preserve provenance. Admin token must be distinct from Agent tokens.
