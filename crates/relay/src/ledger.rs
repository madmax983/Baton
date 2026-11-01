//! The client's ledger: every write a host acknowledged, and the audit that
//! checks they all survived.
//!
//! Each acknowledgement carries the slot and the **epoch** the host served it
//! under. The ledger keeps, per slot, the highest epoch it has accepted (the
//! watermark) and refuses an acknowledgement stamped below it: a host serving
//! a slot at an epoch older than one the client has already seen is a zombie,
//! and its write will not survive (no restore selects an older epoch's data).
//! The server is supposed to fence such a host; this is the client checking.
//!
//! Formal (`verus/src/ledger.rs`): the watermark never decreases; every
//! accepted acknowledgement's epoch is at most its slot's watermark; and the
//! audit partitions the acknowledged writes into present and lost — nothing
//! counted twice, nothing dropped.

use std::collections::{BTreeMap, BTreeSet};

/// One acknowledged write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ack {
    /// The tenant.
    pub tenant: String,
    /// The tenant's own sequence number, from 1.
    pub seq: u64,
    /// The slot the tenant hashes to.
    pub slot: u16,
    /// The epoch the host served the slot under.
    pub epoch: u64,
    /// Which host acknowledged it.
    pub host: String,
    /// When, in milliseconds on the console's clock.
    pub at_ms: u64,
}

/// An acknowledgement refused because its epoch is older than one already
/// seen for its slot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{host} acknowledged slot {slot} at epoch {epoch}, but epoch {watermark} was already seen")]
pub struct Stale {
    /// The host.
    pub host: String,
    /// The slot.
    pub slot: u16,
    /// The epoch it claimed.
    pub epoch: u64,
    /// The highest epoch seen for the slot.
    pub watermark: u64,
}

/// The ledger.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    acks: Vec<Ack>,
    watermark: BTreeMap<u16, u64>,
    stale: Vec<Stale>,
}

/// What the audit found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Audit {
    /// Acknowledged writes.
    pub acked: u64,
    /// Acknowledged writes found on the current owner.
    pub present: u64,
    /// Acknowledged writes not found: data loss.
    pub lost: Vec<Ack>,
    /// Writes found, at or below a tenant's highest acknowledged sequence
    /// number, that the ledger never saw acknowledged: the write landed, the
    /// answer did not reach the client. Not loss. (Writes above the highest
    /// acknowledged sequence were still in flight when the ledger was read,
    /// and are not counted.)
    pub unacked_present: u64,
    /// Tenants the audit could not read.
    pub unreadable: Vec<String>,
}

impl Ledger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept an acknowledgement, unless its epoch is below the slot's
    /// watermark.
    ///
    /// # Errors
    ///
    /// [`Stale`], which is also kept for the console to show.
    pub fn record(&mut self, ack: Ack) -> Result<(), Stale> {
        let w = self.watermark.entry(ack.slot).or_insert(0);
        if ack.epoch < *w {
            let stale = Stale {
                host: ack.host,
                slot: ack.slot,
                epoch: ack.epoch,
                watermark: *w,
            };
            self.stale.push(stale.clone());
            return Err(stale);
        }
        *w = ack.epoch;
        self.acks.push(ack);
        Ok(())
    }

    /// Accepted acknowledgements, in order.
    #[must_use]
    pub fn acks(&self) -> &[Ack] {
        &self.acks
    }

    /// Refused ones.
    #[must_use]
    pub fn stale(&self) -> &[Stale] {
        &self.stale
    }

    /// The highest epoch accepted for `slot`.
    #[must_use]
    pub fn watermark(&self, slot: u16) -> u64 {
        self.watermark.get(&slot).copied().unwrap_or(0)
    }

    /// The tenants that have at least one acknowledged write.
    #[must_use]
    pub fn tenants(&self) -> BTreeSet<String> {
        self.acks.iter().map(|a| a.tenant.clone()).collect()
    }

    /// Check every acknowledged write against what the owners hold now.
    /// `found` maps each tenant read to the sequence numbers present; a
    /// tenant missing from it is unreadable, and its writes count as neither
    /// present nor lost.
    #[must_use]
    pub fn audit(&self, found: &BTreeMap<String, BTreeSet<u64>>) -> Audit {
        let mut a = Audit::default();
        let mut acked_seqs: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
        let mut unreadable = BTreeSet::new();
        for ack in &self.acks {
            let Some(seqs) = found.get(&ack.tenant) else {
                unreadable.insert(ack.tenant.clone());
                continue;
            };
            a.acked += 1;
            if seqs.contains(&ack.seq) {
                a.present += 1;
            } else {
                a.lost.push(ack.clone());
            }
            acked_seqs.entry(&ack.tenant).or_default().insert(ack.seq);
        }
        for (tenant, seqs) in found {
            if let Some(acked) = acked_seqs.get(tenant.as_str()) {
                let top = acked.last().copied().unwrap_or(0);
                a.unacked_present += seqs
                    .iter()
                    .filter(|s| **s <= top && !acked.contains(s))
                    .count() as u64;
            }
        }
        a.unreadable = unreadable.into_iter().collect();
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ack(tenant: &str, seq: u64, slot: u16, epoch: u64) -> Ack {
        Ack {
            tenant: tenant.into(),
            seq,
            slot,
            epoch,
            host: "host-a".into(),
            at_ms: seq,
        }
    }

    #[test]
    fn an_older_epoch_is_refused_and_kept() {
        let mut l = Ledger::new();
        l.record(ack("t", 1, 7, 2)).unwrap();
        l.record(ack("t", 2, 7, 3)).unwrap();
        let e = l.record(ack("t", 3, 7, 2)).unwrap_err();
        assert_eq!((e.epoch, e.watermark), (2, 3));
        assert_eq!(l.acks().len(), 2);
        assert_eq!(l.stale().len(), 1);
        assert_eq!(l.watermark(7), 3);
        // Other slots are unaffected.
        l.record(ack("u", 1, 8, 1)).unwrap();
        assert_eq!(l.watermark(8), 1);
    }

    #[test]
    fn the_audit_counts_present_lost_and_unacknowledged() {
        let mut l = Ledger::new();
        for s in [1, 2, 4, 6] {
            l.record(ack("t", s, 1, 1)).unwrap();
        }
        l.record(ack("u", 1, 2, 1)).unwrap();
        // 3 landed but was never acknowledged; 4 was acknowledged and is
        // gone; 7 is above anything acknowledged (in flight) and is ignored.
        let found = BTreeMap::from([("t".to_owned(), BTreeSet::from([1, 2, 3, 6, 7]))]);
        let a = l.audit(&found);
        assert_eq!((a.acked, a.present), (4, 3));
        assert_eq!(a.lost.iter().map(|x| x.seq).collect::<Vec<_>>(), vec![4]);
        assert_eq!(a.unacked_present, 1);
        assert_eq!(a.unreadable, vec!["u".to_owned()]);
    }

    /// The console audits a snapshot taken before it reads. An ack recorded
    /// after the snapshot is not judged against reads that may predate it.
    #[test]
    fn a_snapshot_does_not_see_later_acks() {
        let mut l = Ledger::new();
        l.record(ack("t", 1, 0, 1)).unwrap();
        let snap = l.clone();
        l.record(ack("t", 2, 0, 1)).unwrap();
        let found = BTreeMap::from([("t".to_owned(), BTreeSet::from([1]))]);
        assert!(snap.audit(&found).lost.is_empty());
        assert_eq!(
            l.audit(&found).lost.len(),
            1,
            "the live ledger would call #2 lost"
        );
    }

    proptest! {
        /// The Verus theorems, run.
        #[test]
        fn watermark_is_monotone_and_bounds_every_ack(
            events in proptest::collection::vec((0u16..6, 0u64..6), 0..200),
        ) {
            let mut l = Ledger::new();
            let mut prev: BTreeMap<u16, u64> = BTreeMap::new();
            for (i, (slot, epoch)) in events.into_iter().enumerate() {
                let _ = l.record(ack("t", i as u64, slot, epoch));
                for s in 0..6 {
                    prop_assert!(l.watermark(s) >= prev.get(&s).copied().unwrap_or(0));
                    prev.insert(s, l.watermark(s));
                }
            }
            for a in l.acks() {
                prop_assert!(a.epoch <= l.watermark(a.slot));
            }
        }

        #[test]
        fn the_audit_partitions_the_acknowledged(
            n in 0u64..60, keep in proptest::collection::vec(any::<bool>(), 60),
        ) {
            let mut l = Ledger::new();
            for s in 1..=n { l.record(ack("t", s, 0, 1)).unwrap(); }
            let present: BTreeSet<u64> = (1..=n).filter(|s| keep[usize::try_from(*s - 1).unwrap()]).collect();
            let a = l.audit(&BTreeMap::from([("t".to_owned(), present.clone())]));
            prop_assert_eq!(a.acked, n);
            prop_assert_eq!(a.present + a.lost.len() as u64, a.acked);
            prop_assert_eq!(a.present, present.len() as u64);
            prop_assert_eq!(a.unacked_present, 0);
        }
    }
}
