# ADR 0002: The client keeps its own epoch watermark and audits a snapshot

Status: accepted, 2026-10-31

## Context

The server fences a superseded owner: it should never acknowledge a write at
an epoch older than the slot's current one. Baton is testing that claim, so
it cannot only take the server's word for it. And "how much did a failover
lose" needs a ledger of what was acknowledged and a way to compare it with
what survived.

## Decision

Every acknowledgement carries `(slot, epoch)`. The client's `Ledger` keeps a
per-slot watermark and refuses — and counts — any acknowledgement below it.
The audit snapshots the ledger, then reads every tenant back from whichever
host serves it, and partitions the snapshot's acknowledgements into present
and lost. The watermark and the partition are modelled in
`verus/src/ledger.rs`.

The snapshot comes first. Auditing the live ledger while the client runs
counts writes acknowledged after a tenant was read as lost (it did, in the
first drill). Writes found above a tenant's highest acknowledged sequence
were in flight, and are not counted as "landed, answer lost".

## Consequences

- "refused as stale" is an independent check on the server's fencing; it
  has read zero in every run.
- A lost write is reported with its host, epoch and acknowledgement time,
  so the RPO can be checked against the failure that caused it.
