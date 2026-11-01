// Baton — the verified core: the client cannot be fooled by a zombie, and it
// counts loss exactly.
//
//   verus verus/src/ledger.rs
//
// The console's client writes through whichever host currently answers for a
// tenant's slot. Each acknowledgement carries the epoch the host served the
// slot under. The ledger keeps, per slot, the highest epoch it has accepted —
// the watermark — and refuses anything below it. This file proves:
//
//   1. the watermark never decreases;
//   2. every accepted acknowledgement's epoch is <= its slot's watermark, at
//      every point, so nothing a superseded owner said is ever counted;
//   3. the audit partitions the acknowledged writes of a readable tenant:
//      present + lost == acked.
//
// ── The modelling assumptions ───────────────────────────────────────────────
//
// The runtime ledger keeps watermarks in a BTreeMap<u16, u64> defaulting to 0;
// here it is a Map<u16, u64> with the same default (`wm`). An acknowledgement
// is abstracted to (slot, epoch) for 1–2, and the audit to a sequence of
// booleans (was this acknowledged write found?) for 3. Tenants, hosts and
// timestamps do not affect any of the three properties.

use vstd::prelude::*;

verus! {

pub open spec fn wm(m: Map<u16, u64>, slot: u16) -> u64 {
    if m.contains_key(slot) { m[slot] } else { 0 }
}

pub struct Ack {
    pub slot: u16,
    pub epoch: u64,
}

pub struct Ledger {
    pub acks: Seq<Ack>,
    pub marks: Map<u16, u64>,
}

/// SPEC 2.1. Every accepted ack is at or below its slot's watermark.
pub open spec fn bounded(l: Ledger) -> bool {
    forall|i: int| 0 <= i < l.acks.len() ==> l.acks[i].epoch <= wm(l.marks, l.acks[i].slot)
}

/// SPEC 2.2. `record`: accept when epoch >= watermark, raising it.
pub open spec fn record(l: Ledger, a: Ack) -> (Ledger, bool) {
    if a.epoch < wm(l.marks, a.slot) {
        (l, false)
    } else {
        (Ledger { acks: l.acks.push(a), marks: l.marks.insert(a.slot, a.epoch) }, true)
    }
}

/// SPEC 2.3. The watermark never goes down, for any slot, on any record.
pub proof fn lemma_watermark_monotone(l: Ledger, a: Ack, s: u16)
    ensures
        wm(record(l, a).0.marks, s) >= wm(l.marks, s),
{
}

/// SPEC 2.4. Recording preserves `bounded`.
pub proof fn lemma_record_preserves_bounded(l: Ledger, a: Ack)
    requires
        bounded(l),
    ensures
        bounded(record(l, a).0),
{
    let (n, ok) = record(l, a);
    if ok {
        assert forall|i: int| 0 <= i < n.acks.len() implies n.acks[i].epoch <= wm(
            n.marks,
            n.acks[i].slot,
        ) by {
            if i < l.acks.len() as int {
                assert(n.acks[i] == l.acks[i]);
                lemma_watermark_monotone(l, a, l.acks[i].slot);
            } else {
                assert(n.acks[i] == a);
            }
        }
    }
}

/// Fold a sequence of acks.
pub open spec fn run(l: Ledger, xs: Seq<Ack>) -> Ledger
    decreases xs.len(),
{
    if xs.len() == 0 { l } else { run(record(l, xs[0]).0, xs.skip(1)) }
}

/// SPEC 2.5. **The theorem.** From an empty ledger, whatever arrives, every
/// counted acknowledgement is at or below its slot's watermark.
pub proof fn theorem_bounded(l: Ledger, xs: Seq<Ack>)
    requires
        bounded(l),
    ensures
        bounded(run(l, xs)),
    decreases xs.len(),
{
    if xs.len() > 0 {
        lemma_record_preserves_bounded(l, xs[0]);
        theorem_bounded(record(l, xs[0]).0, xs.skip(1));
    }
}

/// SPEC 2.6. The executable record, over a vector of watermarks indexed by
/// slot (the runtime's map, densified).
pub fn record_exec(marks: &mut Vec<u64>, slot: u16, epoch: u64) -> (ok: bool)
    requires
        (slot as int) < old(marks)@.len(),
    ensures
        marks@.len() == old(marks)@.len(),
        ok == (epoch >= old(marks)@[slot as int]),
        ok ==> marks@ == old(marks)@.update(slot as int, epoch),
        !ok ==> marks@ == old(marks)@,
        forall|s: int| 0 <= s < marks@.len() ==> marks@[s] >= old(marks)@[s],
{
    let i = slot as usize;
    if epoch < marks[i] {
        false
    } else {
        marks.set(i, epoch);
        true
    }
}

pub open spec fn count_true(found: Seq<bool>) -> nat
    decreases found.len(),
{
    if found.len() == 0 { 0 } else { count_true(found.drop_last()) + if found.last() { 1nat } else { 0nat } }
}

/// SPEC 3.1. The audit's counting loop: one flag per acknowledged write of a
/// readable tenant. present + lost == acked, exactly.
pub fn audit_counts(found: &Vec<bool>) -> (r: (u64, u64))
    requires
        found@.len() <= 1_000_000_000,
    ensures
        r.0 as nat == count_true(found@),
        r.0 + r.1 == found@.len(),
{
    let mut present: u64 = 0;
    let mut lost: u64 = 0;
    let mut i: usize = 0;
    while i < found.len()
        invariant
            i <= found@.len(),
            found@.len() <= 1_000_000_000,
            present as nat == count_true(found@.subrange(0, i as int)),
            present + lost == i,
        decreases found.len() - i,
    {
        assert(found@.subrange(0, i as int + 1).drop_last() =~= found@.subrange(0, i as int));
        assert(found@.subrange(0, i as int + 1).last() == found@[i as int]);
        if found[i] {
            present = present + 1;
        } else {
            lost = lost + 1;
        }
        i = i + 1;
    }
    assert(found@.subrange(0, found@.len() as int) =~= found@);
    (present, lost)
}

fn main() {
}

} // verus!
