#!/usr/bin/env python3
"""The console drill, end to end, over HTTP. Standard library only.

Drives a running console (default http://127.0.0.1:3000) exactly as the
buttons do: start the client, kill -9 host-a, start the standby, audit,
freeze the standby, fail back to host-a, thaw the zombie, audit again.
Exits non-zero if any claim fails:

  * the standby claims the lower half at a newer epoch;
  * every lost write was acknowledged inside the RPO window before the
    failure that lost it (2 s, the hosts' `rpo_secs`);
  * fail-back serves at a newer epoch still;
  * the thawed zombie holds nothing and is waiting;
  * the client never accepted an acknowledgement below a slot's watermark.

    python3 tools/drill.py [base-url]
"""

import http.cookiejar
import json
import re
import sys
import time
import urllib.error
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:3000"
RPO_MS = 2000
LOWER = "0-8191"

jar = http.cookiejar.CookieJar()
op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
page = op.open(BASE + "/").read().decode()
token = re.search(r'name="csrf-token" content="([^"]+)"', page).group(1)
failures = []
started = time.time()


def now_ms():
    return (time.time() - started) * 1000


def post(path):
    req = urllib.request.Request(
        BASE + path, data=b"", method="POST", headers={"x-csrf-token": token}
    )
    with op.open(req, timeout=120) as r:
        return json.load(r)


def state():
    return json.load(op.open(BASE + "/_state.json"))


def host(name):
    return next(h for h in state()["hosts"] if h["name"] == name)


def serving(name):
    for r in host(name)["view"]["held"]:
        if r["range"] == LOWER and r["serving"]:
            return r["epoch"]
    return None


def wait_serving(name, within):
    t = time.time()
    while time.time() - t < within:
        e = serving(name)
        if e is not None:
            return e, time.time() - t
        time.sleep(0.25)
    failures.append(f"{name} never served {LOWER}")
    return None, None


def check(cond, msg):
    print(("  ok   " if cond else "  FAIL ") + msg)
    if not cond:
        failures.append(msg)


def audit(label, failure_at):
    """Audit, and check every lost write was acknowledged inside the RPO
    window before the failure that lost it. Timestamps are console ms."""
    a = post("/audit")
    print(f"{label}: {a['acked']} acknowledged, {a['present']} present, {a['lost']} lost")
    check(not a["unreadable"], f"{label}: every runner readable")
    for lost in a["lost_sample"]:
        before = [f for f in failure_at if f >= lost["at_ms"]]
        gap = (min(before) - lost["at_ms"]) if before else None
        check(
            gap is not None and gap <= RPO_MS,
            f"{label}: {lost['tenant']}#{lost['seq']} ({lost['host']}) lost "
            + (f"{gap:.0f} ms before a failure" if gap is not None else "with no failure after it"),
        )
    return a


ea = serving("host-a")
check(ea is not None, f"host-a serves {LOWER} at boot")
post("/client/start")
time.sleep(6)

base = state()["now_ms"]  # console clock, for the audit's at_ms
t0 = time.time()
post("/hosts/host-a/kill")
kill_at = base + (time.time() - t0) * 1000
post("/hosts/host-c/start")
ec, took = wait_serving("host-c", 40)
if ec is not None:
    print(f"host-c took over in {took:.1f} s at epoch {ec}")
    check(ec > (ea or 0), "the standby claims at a newer epoch")
time.sleep(3)
audit("audit 1", [kill_at])

t1 = time.time()
base = state()["now_ms"]
post("/hosts/host-c/freeze")
freeze_at = base + (time.time() - t1) * 1000
time.sleep(8)
post("/hosts/host-a/start")
ea2, took = wait_serving("host-a", 40)
if ea2 is not None:
    print(f"host-a failed back in {took:.1f} s at epoch {ea2}")
    check(ea2 > (ec or 0), "fail-back claims at a newer epoch still")
time.sleep(2)
post("/hosts/host-c/thaw")
time.sleep(4)
c = host("host-c")["view"]
check(not c["held"], "the thawed zombie holds nothing")
check(LOWER in c["waiting"], "the thawed zombie is waiting to claim")
audit("audit 2", [kill_at, freeze_at])
w = state()["workload"]
check(w["stale"] == 0, "no acknowledgement below a slot's watermark")
post("/client/stop")
print(f"client: {w['acked']} acknowledged, longest outage {w['longest_outage_ms'] / 1000:.1f} s")

if failures:
    print(f"\n{len(failures)} claim(s) failed")
    sys.exit(1)
print("\nthe drill passed")
