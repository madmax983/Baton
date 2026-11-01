# ADR 0001: Hosts are real processes, not in-process fleets

Status: accepted, 2026-10-31

## Context

autumn-web's `TestApp::with_fleet` can build several fleets in one process,
and that would make tests fast. But ownership is per-process configuration
(`[database.fleet.ownership] owner`), leases are renewed by a background task
that dies only when the process dies, and the two failures Baton exists to
show — a hard crash and a paused process that wakes up — are things done to
a process: `kill -9` and SIGSTOP/SIGCONT. An in-process "kill" is a drop,
which runs destructors, releases leases and flushes; it is the clean stop,
not the crash.

## Decision

Every host is `baton-app host`, a child process with its own `autumn.toml`
and working directory. The console supervises them with `kill` (SIGKILL,
SIGTERM, SIGSTOP, SIGCONT), writes a pid file per host, and kills orphans
from an earlier console on boot. The integration test drives the same
supervisor against three hosts on their own ports.

## Consequences

- The failover test takes ~26 s and needs free ports 4101–4103.
- Unix only (`kill`, `ps`). Fine for the lab; the Mac is the target.
- What the test shows is what production would see: a real lease lapse, a
  real restore from the replica, a real zombie.
