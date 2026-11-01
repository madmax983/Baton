# CLAUDE.md — Baton

A failover lab for autumn-web 0.9's slot fleet with ownership and
replication. Read `PITCH.md`, then `docs/SPEC.md`.

- `crates/relay` — pure (no IO, async or clock): `SlotRange`, `lab()` (the
  three hosts), `Ledger` (per-slot epoch watermark) and `Ledger::audit`.
  Rules go here, with property tests.
- `crates/baton-app/src/host.rs` — the host API. Opens the tenant's slot with
  `DatabaseFleet::open` on every request; maps `FleetError::NotOwner` to 421.
- `crates/baton-app/src/console/` — `supervisor.rs` (child processes via
  `kill`), `mod.rs` (poller, workload, audit), `routes.rs`, `console.js`
  (no inline script: CSP is `script-src 'self'`).
- `crates/baton-app/host.toml` — every host's config template. Ownership,
  lease and replication settings live here, not in the root `autumn.toml`
  (which is the console's).
- `verus/src/ledger.rs` — the ledger's model. Change the runtime and the
  model together.

Rules that matter here:

- The audit must snapshot the ledger **before** it reads (see the comment on
  `Console::audit`); auditing the live ledger reports in-flight writes as
  lost.
- Hosts are real processes on purpose (ADR 0001). Do not replace them with
  in-process fleets to make tests faster; `kill -9` and SIGSTOP are the
  point.
- `tests/failover.rs` uses ports 4101–4103 and the console 3000 and
  3101–3103; keep them apart so `cargo test` can run beside a console.
- Never hold a `FleetDatabase` across requests: the epoch is checked on open.
- Host SQL is raw `diesel::sql_query` through `autumn_web::reexports`; there
  are no model macros, so there is no E0034 trap here yet. If you add them,
  import specific derives, not `diesel::prelude::*`.

Gates: `./verify.sh` (authoritative, on the Mac). In the cloud:
`cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings && cargo test --workspace`.
