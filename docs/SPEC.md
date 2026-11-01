# Baton — specification

## 1. The lab

Each host is `baton-app host`, a real process with its own `autumn.toml`
(from `crates/baton-app/host.toml`):

- `[database.fleet] mode = "slot"`, `root` = the host's own volume,
  `max_open = 64`, `restore_missing = true`;
- `[database.fleet.ownership] owner = <name>`, `slots = [<range>]`,
  `lease_secs = 6`;
- `[replication] path = shared/replica`, `rpo_secs = 2`,
  `sync_interval_secs = 1`, `verify_interval_secs = 0`;
- the control database (`shared/control.db`, the ownership table) is shared.

| Host | Port | Claims | Role |
|---|---|---|---|
| host-a | 3101 | 0–8191 | primary |
| host-b | 3102 | 8192–16383 | primary |
| host-c | 3103 | 0–8191 | standby, empty volume |

The host API: `POST /api/write {tenant, seq, body}` and
`GET /api/read?tenant=` both open the tenant's slot through
`DatabaseFleet::open` and answer `{host, slot, epoch, seqs}`; a slot this
host does not serve is `421 Misdirected Request`.

## 2. The ledger (`crates/relay/src/ledger.rs`, `verus/src/ledger.rs`)

Every acknowledgement the client receives carries the slot and the epoch it
was served under. The ledger keeps a per-slot watermark (the highest epoch
accepted) and refuses an acknowledgement below it.

- **2.1** `bounded`: every accepted acknowledgement's epoch ≤ its slot's
  watermark — formal (definition).
- **2.2** `record` accepts iff `epoch ≥ watermark`, and then raises it —
  formal (definition) and `an_older_epoch_is_refused_and_kept`.
- **2.3** The watermark never decreases, for any slot — formal
  (`lemma_watermark_monotone`) and property test
  `watermark_is_monotone_and_bounds_every_ack`.
- **2.4** `record` preserves `bounded` — formal.
- **2.5** From any bounded ledger, any sequence of acknowledgements leaves it
  bounded — formal (`theorem_bounded`), property test as 2.3.
- **2.6** The executable `record_exec` over a dense watermark vector meets
  2.2 and 2.3 — formal.

## 3. The audit

The console snapshots the ledger, then reads every tenant back from
whichever host serves it.

- **3.1** For readable tenants, `present + lost == acked`, exactly — formal
  (`audit_counts`) and property test `the_audit_partitions_the_acknowledged`.
- **3.2** A tenant no host serves is *unreadable*: neither present nor lost —
  `the_audit_counts_present_lost_and_unacknowledged`.
- **3.3** Writes found but never acknowledged are counted only at or below
  the tenant's highest acknowledged sequence; above it they were in flight —
  same test.
- **3.4** An acknowledgement recorded after the snapshot is not judged
  against reads that may predate it — `a_snapshot_does_not_see_later_acks`.

## 4. The fleet's claims, against real processes (`tests/failover.rs`)

One sequential test, three hosts on ports 4101–4103:

| # | Claim |
|---|---|
| 4.1 | The primaries hold their halves at distinct epochs; a tenant on the other half gets 421 from a primary, and the other primary serves it. |
| 4.2 | After `kill -9` of host-a, host-c (empty volume) serves 0–8191 at an epoch above both primaries'. Every tenant's first write (long before the kill) is there, restored from the replica. |
| 4.3 | Every acknowledged write host-c does not have was acknowledged **at most `rpo_secs` (2 s) before the kill**. |
| 4.4 | After freezing host-c and restarting host-a on its stale volume, host-a serves at a newer epoch still, and every write made through host-c (and shipped) is there. |
| 4.5 | Thawed, host-c refuses every write with 421, and none of its attempts reaches the owner. |

The console drill (`tools/drill.py`, run by `verify.sh`) checks the same
claims through the console, plus: the thawed zombie reports *waiting*, and
the client never saw an acknowledgement below a slot's watermark.

## 5. Observed (cloud, debug build)

| | |
|---|---|
| kill -9 → standby serving | 6.3–6.6 s (lease 6 s) |
| acknowledged writes lost on kill -9 | 5–20 per run, all acknowledged 0.4–0.8 s before the kill |
| failover test, 5 s sequential stream | 154 of 1,232 lost, all inside 2 s |
| freeze → lost | 0–5, acknowledged 0.4–0.8 s before the freeze |
| fail-back to a vacated range | 1.0 s |
| client's longest outage (freeze + 8 s + fail-back) | ~11 s |
| acknowledgements below a watermark | 0, every run |

## 6. Scoreboard — the Verus has not been run yet

| Obligation | File | Confidence | Why |
|---|---|---|---|
| 2.3, 2.4 | `ledger.rs` | high | Map `insert` and a two-case `assert forall`; the `wm` default is the only thing to unfold. |
| 2.5 | `ledger.rs` | high | Structural induction on `skip(1)` with a `decreases`. |
| 2.6 | `ledger.rs` | medium-high | `Vec::set` postcondition against `update`; may want an `=~=` on the vector views. |
| 3.1 | `ledger.rs` | medium | Loop invariant over `subrange` with `drop_last`; the two extensionality asserts are there, the arithmetic bound may need one more. |
