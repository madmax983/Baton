//! Baton's engine: slot ranges and the lab's hosts, and the client's ledger
//! of acknowledged writes. No IO, no async, no clock.
//!
//! - [`ranges`] — `A-B` slot ranges and the three hosts of the lab.
//! - [`ledger`] — every acknowledged write, a per-slot epoch watermark that
//!   refuses a zombie host's acknowledgement, and the audit that counts loss
//!   (`verus/src/ledger.rs`).
#![forbid(unsafe_code)]

pub mod ledger;
pub mod ranges;

pub use ledger::{Ack, Audit, Ledger, Stale};
pub use ranges::{HostSpec, RangeError, SLOTS, SlotRange, lab};
