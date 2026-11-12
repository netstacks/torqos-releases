//! torq-secrets: the secrets daemon (ADR-052). Everything is in the library, where its tests are.

// No unsafe code and no dead code, ever (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
// (In a test build, the test harness itself marks `main` `allow(dead_code)`, which `forbid` would reject: there
// it is `deny`, as in the broker. The real binary, which the Nix build compiles, forbids it.)
#![cfg_attr(not(test), forbid(dead_code))]
#![cfg_attr(test, deny(dead_code))]

fn main() -> std::process::ExitCode {
    torq_secrets::run()
}
