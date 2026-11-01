//! The console: the lab's supervisor, its client, and its memory.
//!
//! ```text
//!   buttons ──► supervisor ──► baton-app host ×3 (child processes)
//!   poller  ──► GET /actuator/health on each host every 500 ms ──► timeline
//!   client  ──► POST /api/write to the host that last served the runner;
//!               on 421 / no answer, the next host ──► ledger (epoch watermark)
//!   audit   ──► GET /api/read for every runner ──► present / lost
//! ```

pub mod routes;
pub mod supervisor;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use relay::{Ack, Ledger, SlotRange, lab};
use serde::Serialize;

use crate::host::Served;
use supervisor::{HostProc, Proc};

/// Runners the client writes for.
pub const RUNNERS: usize = 48;
/// The client's pace: one write every this long.
pub const PACE: Duration = Duration::from_millis(20);
/// How long the client waits for one host.
pub const ATTEMPT_TIMEOUT: Duration = Duration::from_millis(800);
/// How often the poller reads every host's health.
pub const POLL: Duration = Duration::from_millis(500);
/// Timeline entries kept.
pub const TIMELINE: usize = 200;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every lock here guards a picture the next poll or write repairs.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A range a host holds, as its health reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Held {
    /// The range.
    pub range: String,
    /// First slot.
    pub start: u16,
    /// Last slot.
    pub end: u16,
    /// Its epoch.
    pub epoch: u64,
    /// Whether the host serves it now (its local lease is valid).
    pub serving: bool,
}

/// What the poller last saw of a host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct HostView {
    /// Whether it answered.
    pub reachable: bool,
    /// Ranges held.
    pub held: Vec<Held>,
    /// Ranges it is waiting to claim.
    pub waiting: Vec<String>,
}

/// A line on the timeline.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    /// Milliseconds since the console started.
    pub at_ms: u64,
    /// `you`, `fleet` or `client`.
    pub kind: &'static str,
    /// What happened.
    pub text: String,
}

/// The client's counters.
#[derive(Debug, Default)]
pub struct Workload {
    running: AtomicBool,
    attempts: AtomicU64,
    acked: AtomicU64,
    misdirected: AtomicU64,
    unanswered: AtomicU64,
    unavailable: AtomicU64,
    stale: AtomicU64,
    longest_outage_ms: AtomicU64,
}

/// The audit, as the page shows it.
#[derive(Debug, Clone, Serialize)]
pub struct AuditView {
    /// When it ran.
    pub at_ms: u64,
    /// Acknowledged writes checked.
    pub acked: u64,
    /// Found.
    pub present: u64,
    /// Not found.
    pub lost: u64,
    /// Lost writes, oldest first (at most 40).
    pub lost_sample: Vec<LostView>,
    /// How long before the audit the newest and oldest lost writes were
    /// acknowledged, in ms.
    pub lost_window_ms: Option<(u64, u64)>,
    /// Writes found that were never acknowledged (landed, answer lost).
    pub unacked_present: u64,
    /// Runners no host would serve.
    pub unreadable: Vec<String>,
}

/// One lost write.
#[derive(Debug, Clone, Serialize)]
pub struct LostView {
    /// The runner.
    pub tenant: String,
    /// Its sequence number.
    pub seq: u64,
    /// Who acknowledged it.
    pub host: String,
    /// Under which epoch.
    pub epoch: u64,
    /// When (ms since the console started).
    pub at_ms: u64,
}

/// Everything the page draws.
#[derive(Debug, Clone, Serialize)]
pub struct State {
    /// Milliseconds since the console started.
    pub now_ms: u64,
    /// The hosts.
    pub hosts: Vec<HostState>,
    /// The client.
    pub workload: WorkloadView,
    /// The last audit.
    pub audit: Option<AuditView>,
    /// Newest first.
    pub timeline: Vec<Event>,
}

/// A host on the page.
#[derive(Debug, Clone, Serialize)]
pub struct HostState {
    /// Name.
    pub name: &'static str,
    /// Role.
    pub role: &'static str,
    /// Port.
    pub port: u16,
    /// What it claims at boot.
    pub slots: String,
    /// `stopped`, `running` or `frozen`.
    pub process: &'static str,
    /// Its pid.
    pub pid: Option<u32>,
    /// What its health said.
    pub view: HostView,
}

/// The client on the page.
#[derive(Debug, Clone, Serialize)]
pub struct WorkloadView {
    /// Writing now.
    pub running: bool,
    /// Writes tried (each host tried counts once).
    pub attempts: u64,
    /// Acknowledged and accepted by the ledger.
    pub acked: u64,
    /// 421s.
    pub misdirected: u64,
    /// No answer in time.
    pub unanswered: u64,
    /// Writes no host would take.
    pub unavailable: u64,
    /// Acknowledgements refused by the epoch watermark.
    pub stale: u64,
    /// The longest a runner went without an acknowledged write, in ms.
    pub longest_outage_ms: u64,
}

/// The console.
#[derive(Debug)]
pub struct Console {
    lab: PathBuf,
    exe: PathBuf,
    secret: String,
    started: Instant,
    hosts: Mutex<Vec<HostProc>>,
    views: Mutex<Vec<HostView>>,
    ledger: Mutex<Ledger>,
    workload: Workload,
    timeline: Mutex<VecDeque<Event>>,
    audit: Mutex<Option<AuditView>>,
    client: reqwest::Client,
}

/// Why a console action failed.
#[derive(Debug, thiserror::Error)]
pub enum ConsoleError {
    /// No such host.
    #[error("no host called {0}")]
    NoHost(String),
    /// No such action.
    #[error("no action called {0}")]
    NoAction(String),
    /// The supervisor refused.
    #[error(transparent)]
    Supervisor(#[from] supervisor::SupervisorError),
    /// The lab directory.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Console {
    /// A console for the lab in `lab`, starting hosts with `exe`.
    ///
    /// # Errors
    ///
    /// The HTTP client could not be built.
    pub fn new(lab: PathBuf, exe: PathBuf, secret: String) -> Result<Self, reqwest::Error> {
        Ok(Self {
            lab,
            exe,
            secret,
            started: Instant::now(),
            hosts: Mutex::new(relay::lab().into_iter().map(HostProc::new).collect()),
            views: Mutex::new(vec![HostView::default(); relay::lab().len()]),
            ledger: Mutex::new(Ledger::new()),
            workload: Workload::default(),
            timeline: Mutex::new(VecDeque::new()),
            audit: Mutex::new(None),
            client: reqwest::Client::builder()
                .timeout(ATTEMPT_TIMEOUT)
                .no_proxy()
                .build()?,
        })
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Add a timeline line.
    pub fn event(&self, kind: &'static str, text: impl Into<String>) {
        let mut t = lock(&self.timeline);
        t.push_front(Event {
            at_ms: self.now_ms(),
            kind,
            text: text.into(),
        });
        t.truncate(TIMELINE);
    }

    /// Run `action` on host `name`: `start`, `kill`, `term`, `freeze`, `thaw`.
    ///
    /// # Errors
    ///
    /// Unknown host or action, or the supervisor refused.
    pub fn act(&self, name: &str, action: &str) -> Result<(), ConsoleError> {
        let mut hosts = lock(&self.hosts);
        let h = hosts
            .iter_mut()
            .find(|h| h.spec.name == name)
            .ok_or_else(|| ConsoleError::NoHost(name.to_owned()))?;
        let what = match action {
            "start" => {
                h.start(&self.exe, &self.lab, &self.secret)?;
                "started"
            }
            "kill" => {
                h.kill()?;
                "killed with SIGKILL (nothing more ships)"
            }
            "term" => {
                h.term()?;
                "sent SIGTERM (ships, releases its ranges, exits)"
            }
            "freeze" => {
                h.freeze()?;
                "frozen with SIGSTOP"
            }
            "thaw" => {
                h.thaw()?;
                "thawed with SIGCONT"
            }
            other => return Err(ConsoleError::NoAction(other.to_owned())),
        };
        drop(hosts);
        self.event("you", format!("{name} {what}"));
        Ok(())
    }

    /// Stop every host, wipe the lab, forget everything, and start the two
    /// primaries.
    ///
    /// # Errors
    ///
    /// The lab directory could not be removed or a host could not start.
    pub fn reset(&self) -> Result<(), ConsoleError> {
        self.workload.running.store(false, Ordering::Release);
        {
            let mut hosts = lock(&self.hosts);
            for h in hosts.iter_mut() {
                if h.proc() != Proc::Stopped {
                    let _ = h.kill();
                }
            }
        }
        if self.lab.exists() {
            std::fs::remove_dir_all(&self.lab)?;
        }
        *lock(&self.ledger) = Ledger::new();
        *lock(&self.audit) = None;
        lock(&self.timeline).clear();
        for c in [
            &self.workload.attempts,
            &self.workload.acked,
            &self.workload.misdirected,
            &self.workload.unanswered,
            &self.workload.unavailable,
            &self.workload.stale,
            &self.workload.longest_outage_ms,
        ] {
            c.store(0, Ordering::Relaxed);
        }
        self.event("you", "reset the lab");
        self.act("host-a", "start")?;
        self.act("host-b", "start")?;
        Ok(())
    }

    /// Stop every host cleanly (console shutdown).
    pub fn stop_all(&self) {
        let mut hosts = lock(&self.hosts);
        for h in hosts.iter_mut() {
            match h.proc() {
                Proc::Running => {
                    let _ = h.term();
                }
                Proc::Frozen => {
                    let _ = h.kill();
                }
                Proc::Stopped => {}
            }
        }
    }

    /// Start or stop the client.
    pub fn set_workload(self: &Arc<Self>, on: bool) {
        let was = self.workload.running.swap(on, Ordering::AcqRel);
        if on && !was {
            self.event("you", "started the client");
            let me = Arc::clone(self);
            tokio::spawn(async move { me.run_workload().await });
        } else if !on && was {
            self.event("you", "stopped the client");
        }
    }

    async fn run_workload(self: Arc<Self>) {
        let ports: Vec<(&'static str, u16)> = lab().iter().map(|h| (h.name, h.port)).collect();
        let mut seq = vec![0u64; RUNNERS];
        let mut preferred: Vec<Option<usize>> = vec![None; RUNNERS];
        let mut down_since: Vec<Option<Instant>> = vec![None; RUNNERS];
        let mut n = 0usize;
        let mut tick = tokio::time::interval(PACE);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while self.workload.running.load(Ordering::Acquire) {
            tick.tick().await;
            let r = n % RUNNERS;
            n += 1;
            let tenant = format!("runner-{r:03}");
            let next = seq[r] + 1;
            let order: Vec<usize> = preferred[r]
                .into_iter()
                .chain((0..ports.len()).filter(|i| Some(*i) != preferred[r]))
                .collect();
            let mut landed = false;
            for i in order {
                let (name, port) = ports[i];
                self.workload.attempts.fetch_add(1, Ordering::Relaxed);
                match self.write(port, &tenant, next).await {
                    Attempt::Served(s) => {
                        let ack = Ack {
                            tenant: tenant.clone(),
                            seq: next,
                            slot: s.slot,
                            epoch: s.epoch,
                            host: s.host.clone(),
                            at_ms: self.now_ms(),
                        };
                        let stale = lock(&self.ledger).record(ack);
                        match stale {
                            Ok(()) => {
                                self.workload.acked.fetch_add(1, Ordering::Relaxed);
                                seq[r] = next;
                                preferred[r] = Some(i);
                                landed = true;
                                if let Some(since) = down_since[r].take() {
                                    let ms = u64::try_from(since.elapsed().as_millis())
                                        .unwrap_or(u64::MAX);
                                    self.workload
                                        .longest_outage_ms
                                        .fetch_max(ms, Ordering::Relaxed);
                                }
                            }
                            Err(e) => {
                                self.workload.stale.fetch_add(1, Ordering::Relaxed);
                                self.event("client", format!("refused {name}'s answer: {e}"));
                                continue;
                            }
                        }
                        break;
                    }
                    Attempt::Misdirected => {
                        self.workload.misdirected.fetch_add(1, Ordering::Relaxed);
                    }
                    Attempt::NoAnswer => {
                        self.workload.unanswered.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            if !landed {
                self.workload.unavailable.fetch_add(1, Ordering::Relaxed);
                if down_since[r].is_none() {
                    down_since[r] = Some(Instant::now());
                }
            }
        }
    }

    async fn write(&self, port: u16, tenant: &str, seq: u64) -> Attempt {
        let url = format!("http://127.0.0.1:{port}/api/write");
        let body = crate::host::WriteReq {
            tenant: tenant.to_owned(),
            seq,
            body: format!("{tenant} lap {seq}"),
        };
        match self.client.post(url).json(&body).send().await {
            Ok(r) if r.status().is_success() => r
                .json::<Served>()
                .await
                .map_or(Attempt::NoAnswer, Attempt::Served),
            Ok(r) if r.status().as_u16() == 421 => Attempt::Misdirected,
            _ => Attempt::NoAnswer,
        }
    }

    /// Read every runner from whichever host serves it, and audit the ledger.
    ///
    /// The ledger is snapshotted **before** the first read, so every audited
    /// acknowledgement happened before the read that checks it. Auditing the
    /// live ledger instead would count writes acknowledged while the audit was
    /// reading as lost.
    pub async fn audit(&self) -> AuditView {
        let snapshot = lock(&self.ledger).clone();
        let tenants = snapshot.tenants();
        let mut found: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
        for t in &tenants {
            for h in lab() {
                let url = format!("http://127.0.0.1:{}/api/read", h.port);
                let Ok(r) = self.client.get(url).query(&[("tenant", t)]).send().await else {
                    continue;
                };
                if r.status().is_success()
                    && let Ok(s) = r.json::<Served>().await
                {
                    found.insert(t.clone(), s.seqs.into_iter().collect());
                    break;
                }
            }
        }
        let now = self.now_ms();
        let a = snapshot.audit(&found);
        let window = match (a.lost.first(), a.lost.last()) {
            (Some(first), Some(last)) => Some((
                now.saturating_sub(last.at_ms),
                now.saturating_sub(first.at_ms),
            )),
            _ => None,
        };
        let view = AuditView {
            at_ms: now,
            acked: a.acked,
            present: a.present,
            lost: a.lost.len() as u64,
            lost_sample: a
                .lost
                .iter()
                .take(40)
                .map(|l| LostView {
                    tenant: l.tenant.clone(),
                    seq: l.seq,
                    host: l.host.clone(),
                    epoch: l.epoch,
                    at_ms: l.at_ms,
                })
                .collect(),
            lost_window_ms: window,
            unacked_present: a.unacked_present,
            unreadable: a.unreadable,
        };
        self.event(
            "you",
            format!(
                "audited: {} acknowledged, {} present, {} lost",
                view.acked, view.present, view.lost
            ),
        );
        *lock(&self.audit) = Some(view.clone());
        view
    }

    /// Poll every host's health once, and note what changed.
    pub async fn poll(&self) {
        let specs = lab();
        let mut fresh = Vec::with_capacity(specs.len());
        for h in &specs {
            fresh.push(self.health(h.port).await);
        }
        let old = std::mem::replace(&mut *lock(&self.views), fresh.clone());
        for ((spec, before), after) in specs.iter().zip(old).zip(fresh) {
            for e in diff(spec.name, &before, &after) {
                self.event("fleet", e);
            }
        }
    }

    async fn health(&self, port: u16) -> HostView {
        let url = format!("http://127.0.0.1:{port}/actuator/health");
        let Ok(r) = self.client.get(url).send().await else {
            return HostView::default();
        };
        let Ok(v) = r.json::<serde_json::Value>().await else {
            return HostView::default();
        };
        parse_health(&v)
    }

    /// Everything the page draws.
    pub fn state(&self) -> State {
        let views = lock(&self.views).clone();
        let hosts = lock(&self.hosts)
            .iter_mut()
            .zip(views)
            .map(|(h, view)| HostState {
                name: h.spec.name,
                role: h.spec.role,
                port: h.spec.port,
                slots: h.spec.slots.to_string(),
                process: h.proc().label(),
                pid: h.pid(),
                view,
            })
            .collect();
        let w = &self.workload;
        State {
            now_ms: self.now_ms(),
            hosts,
            workload: WorkloadView {
                running: w.running.load(Ordering::Acquire),
                attempts: w.attempts.load(Ordering::Relaxed),
                acked: w.acked.load(Ordering::Relaxed),
                misdirected: w.misdirected.load(Ordering::Relaxed),
                unanswered: w.unanswered.load(Ordering::Relaxed),
                unavailable: w.unavailable.load(Ordering::Relaxed),
                stale: w.stale.load(Ordering::Relaxed),
                longest_outage_ms: w.longest_outage_ms.load(Ordering::Relaxed),
            },
            audit: lock(&self.audit).clone(),
            timeline: lock(&self.timeline).iter().cloned().collect(),
        }
    }
}

/// The development signing secret handed to hosts when none is set.
const DEV_SECRET: &str = "baton-lab-development-secret-not-for-production-use";

/// The console's startup hook: a fresh lab with the two primaries running, a
/// poller, and clean shutdown of every host on Ctrl-C.
///
/// # Errors
///
/// The executable or the lab directory could not be found, or a host could
/// not start.
#[allow(clippy::unused_async, reason = "on_startup takes an async hook")]
pub async fn boot(state: autumn_web::AppState) -> autumn_web::AutumnResult<()> {
    use autumn_web::error::AutumnError;
    let fail = |e: &dyn std::fmt::Display| AutumnError::service_unavailable_msg(e.to_string());
    let lab = std::env::current_dir()
        .map_err(|e| fail(&e))?
        .join(std::env::var("BATON_LAB").unwrap_or_else(|_| "baton-lab".to_owned()));
    let exe = std::env::current_exe().map_err(|e| fail(&e))?;
    let secret =
        std::env::var("AUTUMN_SECURITY__SIGNING_SECRET").unwrap_or_else(|_| DEV_SECRET.to_owned());
    let console = Console::new(lab.clone(), exe, secret).map_err(|e| fail(&e))?;
    for h in lock(&console.hosts).iter() {
        if let Some(pid) = h.reap_orphan(&lab) {
            tracing::warn!(
                host = h.spec.name,
                pid,
                "killed a host left running by an earlier console"
            );
        }
    }
    console.reset().map_err(|e| fail(&e))?;
    state.insert_extension(console);
    let console = state
        .extension::<Console>()
        .ok_or_else(|| fail(&"the console did not install"))?;

    let poller = Arc::clone(&console);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(POLL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            poller.poll().await;
        }
    });
    let stopper = Arc::clone(&console);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            stopper.set_workload(false);
            stopper.stop_all();
        }
    });
    tracing::info!(lab = %lab.display(), "Baton console ready: host-a and host-b are starting");
    Ok(())
}

enum Attempt {
    Served(Served),
    Misdirected,
    NoAnswer,
}

/// Read a host's `/actuator/health` body.
#[must_use]
pub fn parse_health(v: &serde_json::Value) -> HostView {
    let d = &v["components"]["fleet:ownership"]["details"];
    let held = d["held"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|h| {
                    let range = h["range"].as_str()?.to_owned();
                    let r = SlotRange::parse(&range).ok()?;
                    Some(Held {
                        start: r.start(),
                        end: r.end(),
                        range,
                        epoch: h["epoch"].as_u64()?,
                        serving: h["serving"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let waiting = d["waiting"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|w| w.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    HostView {
        reachable: true,
        held,
        waiting,
    }
}

/// What changed between two polls of one host, as timeline lines.
#[must_use]
pub fn diff(name: &str, before: &HostView, after: &HostView) -> Vec<String> {
    let mut out = Vec::new();
    match (before.reachable, after.reachable) {
        (true, false) => out.push(format!("{name} stopped answering")),
        (false, true) => out.push(format!("{name} is answering")),
        _ => {}
    }
    for h in &after.held {
        let had = before
            .held
            .iter()
            .find(|b| b.range == h.range && b.epoch == h.epoch);
        match had {
            None => out.push(format!("{name} holds {} at epoch {}", h.range, h.epoch)),
            Some(b) if b.serving && !h.serving => {
                out.push(format!(
                    "{name} stopped serving {} (local lease ran out)",
                    h.range
                ));
            }
            Some(b) if !b.serving && h.serving => {
                out.push(format!("{name} serves {} again", h.range));
            }
            Some(_) => {}
        }
    }
    if after.reachable {
        for b in &before.held {
            if !after
                .held
                .iter()
                .any(|h| h.range == b.range && h.epoch == b.epoch)
            {
                out.push(format!("{name} let go of {} (epoch {})", b.range, b.epoch));
            }
        }
        for w in &after.waiting {
            if !before.waiting.contains(w) {
                out.push(format!("{name} is waiting to claim {w}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(held: &[(&str, u64, bool)], waiting: &[&str]) -> HostView {
        HostView {
            reachable: true,
            held: held
                .iter()
                .map(|(r, e, s)| {
                    let x = SlotRange::parse(r).unwrap();
                    Held {
                        range: (*r).into(),
                        start: x.start(),
                        end: x.end(),
                        epoch: *e,
                        serving: *s,
                    }
                })
                .collect(),
            waiting: waiting.iter().map(|w| (*w).to_owned()).collect(),
        }
    }

    #[test]
    fn parses_the_ownership_details() {
        let v = serde_json::json!({"components":{"fleet:ownership":{"details":{
            "owner":"host-a","held":[{"range":"0-8191","epoch":4,"serving":true}],
            "waiting":["8192-16383"],"lost":[],"failed":[]}}}});
        let h = parse_health(&v);
        assert!(h.reachable);
        assert_eq!(h.held, view(&[("0-8191", 4, true)], &[]).held);
        assert_eq!(h.waiting, vec!["8192-16383".to_owned()]);
    }

    #[test]
    fn the_timeline_names_claims_losses_and_lapses() {
        let empty = HostView::default();
        let a1 = view(&[("0-8191", 1, true)], &[]);
        assert_eq!(
            diff("host-a", &empty, &a1),
            vec!["host-a is answering", "host-a holds 0-8191 at epoch 1"]
        );
        let lapsed = view(&[("0-8191", 1, false)], &[]);
        assert_eq!(
            diff("host-a", &a1, &lapsed),
            vec!["host-a stopped serving 0-8191 (local lease ran out)"]
        );
        let lost = view(&[], &["0-8191"]);
        assert_eq!(
            diff("host-a", &lapsed, &lost),
            vec![
                "host-a let go of 0-8191 (epoch 1)",
                "host-a is waiting to claim 0-8191"
            ]
        );
        assert_eq!(
            diff("host-a", &a1, &HostView::default()),
            vec!["host-a stopped answering"]
        );
        let reclaimed = view(&[("0-8191", 5, true)], &[]);
        assert_eq!(
            diff("host-a", &lost, &reclaimed),
            vec!["host-a holds 0-8191 at epoch 5"]
        );
    }
}
