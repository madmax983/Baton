# Baton

**Pass the slots, drop nothing — and count what you drop when you can't.**
A failover lab for autumn-web 0.9's slot fleet with
`[database.fleet.ownership]` and `[replication]` on.

Three real host processes share one control database (where the ownership
table lives) and one replica directory. host-a and host-b split the 16,384
slots under leases; host-c is a standby for the lower half with an empty
volume. A console starts and kills them, writes through them as a client
would, and audits every write a host ever acknowledged.

## The moment

The client is running: 48 runners, a write every 20 ms each. You press
**kill -9** on host-a and **Start** on host-c. host-c's lane shows a dashed
"waiting for 0-8191" for six seconds — host-a's lease running out — and then
it goes solid green at epoch 3. The client's 421 and no-answer counters stop
climbing.

You press **Audit every write**. *709 acknowledged, 689 present, 20 lost.*
The audit lists them: every one was acknowledged by host-a, at epoch 2, in
the 0.8 s before you killed it — the writes that had not shipped to the
replica yet. That is the RPO, measured, not quoted.

Then you freeze host-c with SIGSTOP, start host-a again on its stale volume,
and watch it take the range back in a second at epoch 4, restoring host-c's
writes from the replica before it serves. Thaw host-c: it wakes up believing
it owns half the slots, and every write it is offered comes back 421. The
client's "refused as stale" counter — its own epoch check, independent of
the server — stays at zero.

## Where it could go

- Rebalancing a live range (move 4096–8191 from host-a to host-b without a
  kill) once the fleet has an API for handing a range over.
- A second replica destination, and a takeover from the farther one.
- Clock skew: run a host under `faketime` and see if lease expiry holds.
