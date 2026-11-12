//! What crosses between TorqOS's programs (ADR-044): the broker's socket, and later its ledger's words.
//! The broker, torqcli and the agent each take them from here, so a word added on one side is added on
//! all of them, and the compiler says where it is not handled. Nothing of the broker's logic is here.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

pub mod desktop;
pub mod secrets;
pub mod socket;
