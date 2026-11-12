//! `torq` and `torqcli`: the program. Its work is the library's (src/lib.rs), which TorqOS Settings shares.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020). (In a test build, the test harness itself
// marks `main` `allow(dead_code)`, which `forbid` would reject: there it is `deny`. The real binary,
// which the Nix build compiles, forbids it, so an `allow(dead_code)` in the source never builds.)
#![forbid(unsafe_code)]
#![cfg_attr(not(test), forbid(dead_code))]
#![cfg_attr(test, deny(dead_code))]

fn main() {
    torq::main();
}
