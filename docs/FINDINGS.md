# Findings: autumn-web 0.9's slot ownership and replication, from building Baton

Apiary's and Pigeonhole's `docs/FINDINGS.md` cover the fleet itself (the soft
`max_open`, the slow-query attribution, the Postgres-flavoured connection
warning). This is about ownership and replication. Short version: **the
fencing holds and the RPO is honest.** Nothing here is a bug; items 1 and 2
are things the 0.9 docs should say out loud.

**Status:** fixed in the docs by
[autumn-foundation/autumn#3564](https://github.com/autumn-foundation/autumn/pull/3564).
The SQLite fleet guide has a new section, "What a failover costs". Tests
in autumn pin each claim. Each item below gives its status. Item 6 is
narrower than first written.

1. **An acknowledged write can be lost on failover, by design, and the bound
   is real.** Replication is asynchronous: a write is durable on the owner's
   volume when acknowledged and reaches the replica on the next sync. When
   the owner dies, a standby on another volume restores from the replica and
   the unshipped tail is gone. Measured with `rpo_secs = 2`,
   `sync_interval_secs = 1`: every lost write, in every run, was
   acknowledged 0.4–0.8 s before the failure, never more than 2 s. The guide
   should state "acknowledged ≠ survives a failover to another volume; the
   gap is bounded by `rpo_secs`" next to `restore_missing`, because the word
   *replication* makes people assume otherwise.
   *Status: documented. The Replication section says "asynchronous" next to
   `restore_missing`. The cost table gives the bound.*

2. **Takeover time is the lease.** A standby waiting on a dead owner's range
   serves 6.3–6.6 s after the kill with `lease_secs = 6`; a vacated range
   (the old owner frozen past its lease) is claimed in about a second. The
   availability cost of a long lease is a long outage after a hard failure;
   worth a sentence in the ownership guide alongside the default.
   *Status: documented. The bound is `lease_secs + lease_secs / 3` after the
   last renew: 8 s here and 40 s by default. A vacated range takes up to
   `lease_secs / 3`. A note next to the default links to the table.*

3. **Fencing works against a real zombie.** A host frozen with SIGSTOP past
   its lease, thawed after another host claimed its range at a newer epoch,
   refuses every write with 421 (`FleetError::NotOwner`), reports the range
   under `waiting` in `fleet:ownership`, and none of its attempts reaches the
   new owner. The client's own epoch watermark refused nothing in any run:
   no stale-epoch acknowledgement ever got out.
   *Status: no change needed. The guide says that the range shows as
   `waiting`.*

4. **Fail-back to a stale volume restores the newer data.** host-a restarted
   on its own old volume, after host-c had served and written its range, and
   served host-c's writes — the replica's newer copy replaced the stale local
   file rather than the local file being trusted. Good; worth stating,
   because "restore_missing" reads as "only when the file is absent".
   *Status: documented. The restore of a stale file does not need
   `restore_missing`. A test pins this.*

5. **`fleet:ownership` in the detailed health is enough to build a console
   on.** owner, held ranges with epoch and serving, waiting, lost, failed —
   the whole Baton console is drawn from it, polled every 500 ms.
   *Status: no change needed.*

6. **Lab note: a freeze loses the unshipped tail too.** SIGSTOP is not a
   clean stop; writes acknowledged in the second before the freeze were lost
   on fail-back exactly as on `kill -9` (0–5 per run).
   *Status: documented, and narrower. A paused host loses the tail only of
   the databases that the new owner opened during the pause. When it
   continues, it ships the other writes. A crash loses all of the tail.*
