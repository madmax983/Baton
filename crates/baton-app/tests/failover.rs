//! The lab's claims, against real host processes.
//!
//! Three `baton-app host` processes on spare ports, one shared control
//! database and replica, driven through the console's own supervisor. One
//! test on purpose: the steps are a sequence, and each depends on the last.
//!
//! What it holds the framework to:
//!   1. two primaries split the slots and answer 421 for the other's slots;
//!   2. after `kill -9`, a standby on an empty volume serves the dead host's
//!      range at a higher epoch, restored from the replica, and the only
//!      acknowledged writes missing are ones acknowledged inside the RPO
//!      window before the kill;
//!   3. fail-back to the old host's stale volume restores the standby's
//!      writes before serving;
//!   4. a frozen owner, thawed after its range moved, refuses every write.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use baton_app::console::parse_health;
use baton_app::console::supervisor::HostProc;
use baton_app::host::{Served, WriteReq};
use relay::{HostSpec, SlotRange};

const RPO: Duration = Duration::from_secs(2);
/// Generous: a cold debug host claims in well under ten seconds.
const CLAIM: Duration = Duration::from_mins(1);
const SECRET: &str = "baton-test-secret-not-for-production-use-0123456789";

fn spec(name: &'static str, a: u16, b: u16, port: u16) -> HostSpec {
    HostSpec {
        name,
        slots: SlotRange::new(a, b).unwrap(),
        port,
        role: "test",
    }
}

struct Lab {
    client: reqwest::Client,
    hosts: BTreeMap<&'static str, HostProc>,
    root: std::path::PathBuf,
}

impl Lab {
    fn new(root: &Path) -> Self {
        let hosts = [
            spec("host-a", 0, 8_191, 4101),
            spec("host-b", 8_192, 16_383, 4102),
            spec("host-c", 0, 8_191, 4103),
        ]
        .into_iter()
        .map(|s| (s.name, HostProc::new(s)))
        .collect();
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .no_proxy()
                .build()
                .unwrap(),
            hosts,
            root: root.to_path_buf(),
        }
    }

    fn port(&self, name: &str) -> u16 {
        self.hosts[name].spec.port
    }

    fn start(&mut self, name: &str) {
        let exe = Path::new(env!("CARGO_BIN_EXE_baton-app"));
        let root = self.root.clone();
        self.hosts
            .get_mut(name)
            .unwrap()
            .start(exe, &root, SECRET)
            .unwrap();
    }

    async fn write(&self, name: &str, tenant: &str, seq: u64) -> Result<Served, u16> {
        let url = format!("http://127.0.0.1:{}/api/write", self.port(name));
        let body = WriteReq {
            tenant: tenant.into(),
            seq,
            body: "lap".into(),
        };
        match self.client.post(url).json(&body).send().await {
            Ok(r) if r.status().is_success() => Ok(r.json().await.unwrap()),
            Ok(r) => Err(r.status().as_u16()),
            Err(_) => Err(0),
        }
    }

    async fn read(&self, name: &str, tenant: &str) -> Result<Served, u16> {
        let url = format!("http://127.0.0.1:{}/api/read", self.port(name));
        match self
            .client
            .get(url)
            .query(&[("tenant", tenant)])
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => Ok(r.json().await.unwrap()),
            Ok(r) => Err(r.status().as_u16()),
            Err(_) => Err(0),
        }
    }

    /// Wait until `name` holds and serves `range`; its epoch.
    async fn serving(&self, name: &str, range: &str, within: Duration) -> u64 {
        let started = Instant::now();
        loop {
            let url = format!("http://127.0.0.1:{}/actuator/health", self.port(name));
            if let Ok(r) = self.client.get(url).send().await
                && let Ok(v) = r.json::<serde_json::Value>().await
                && let Some(h) = parse_health(&v)
                    .held
                    .iter()
                    .find(|h| h.range == range && h.serving)
            {
                return h.epoch;
            }
            assert!(started.elapsed() < within, "{name} never served {range}");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        for h in self.hosts.values_mut() {
            let _ = h.kill();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one drill, each step depends on the last"
)]
async fn failover_failback_and_fencing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut lab = Lab::new(tmp.path());
    lab.start("host-a");
    lab.start("host-b");
    let ea = lab.serving("host-a", "0-8191", CLAIM).await;
    let eb = lab.serving("host-b", "8192-16383", CLAIM).await;
    assert_ne!(ea, eb);

    // ── 1. Split, and 421 across the split. ──────────────────────────────
    let mut lower = Vec::new();
    for i in 0..40 {
        let t = format!("runner-{i:03}");
        match lab.write("host-a", &t, 1).await {
            Ok(s) => {
                assert!(s.slot < 8_192 && s.epoch == ea && s.host == "host-a");
                lower.push(t);
            }
            Err(code) => {
                assert_eq!(code, 421, "{t}");
                let s = lab.write("host-b", &t, 1).await.unwrap();
                assert!(s.slot >= 8_192 && s.epoch == eb);
            }
        }
    }
    assert!(
        lower.len() >= 10,
        "too few lower-half runners: {}",
        lower.len()
    );

    // ── 2. A steady stream, then kill -9 host-a. ─────────────────────────
    let mut acked: Vec<(String, u64, Instant)> = Vec::new();
    let mut seq = 1u64;
    let stream_until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < stream_until {
        seq += 1;
        for t in &lower {
            if lab.write("host-a", t, seq).await.is_ok() {
                acked.push((t.clone(), seq, Instant::now()));
            }
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let killed = Instant::now();
    lab.hosts.get_mut("host-a").unwrap().kill().unwrap();
    lab.start("host-c");
    let ec = lab.serving("host-c", "0-8191", CLAIM).await;
    assert!(
        ec > ea.max(eb),
        "the standby claims at a newer epoch ({ec} vs {ea})"
    );

    let mut lost = 0;
    for t in &lower {
        let have = lab.read("host-c", t).await.unwrap();
        assert_eq!(have.epoch, ec);
        assert!(
            have.seqs.contains(&1),
            "{t}#1 was written long before the kill"
        );
        for (_, s, at) in acked.iter().filter(|(x, _, _)| x == t) {
            if !have.seqs.contains(s) {
                lost += 1;
                let age = killed.duration_since(*at);
                assert!(
                    age <= RPO,
                    "{t}#{s} acknowledged {age:?} before the kill was lost"
                );
            }
        }
    }
    eprintln!(
        "kill -9: {lost} of {} acknowledged writes lost, all inside the RPO",
        acked.len()
    );

    // ── 3. Write through the standby, freeze it, fail back. ──────────────
    for t in &lower {
        lab.write("host-c", t, 500).await.unwrap();
    }
    tokio::time::sleep(RPO + Duration::from_secs(1)).await; // let it ship
    lab.hosts.get_mut("host-c").unwrap().freeze().unwrap();
    tokio::time::sleep(Duration::from_secs(8)).await; // its 6 s lease lapses
    lab.start("host-a");
    let ea2 = lab.serving("host-a", "0-8191", CLAIM).await;
    assert!(ea2 > ec);
    for t in &lower {
        let have = lab.read("host-a", t).await.unwrap();
        assert!(
            have.seqs.contains(&500),
            "{t}#500, written through host-c, was not restored"
        );
    }

    // ── 4. Thaw the zombie: every write refused. ─────────────────────────
    lab.hosts.get_mut("host-c").unwrap().thaw().unwrap();
    for (k, t) in lower.iter().enumerate() {
        let r = lab.write("host-c", t, 900 + k as u64).await;
        assert!(matches!(r, Err(421)), "the zombie answered {r:?} for {t}");
    }
    for t in &lower {
        let have = lab.read("host-a", t).await.unwrap();
        assert!(
            have.seqs.iter().all(|s| *s < 900),
            "a zombie write reached the owner: {t}"
        );
    }
}
