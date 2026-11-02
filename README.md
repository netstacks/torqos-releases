# TorqOS

A thin, CLI-only operating system built on NixOS, where **the configuration is the device**.

One configuration tree, every NixOS setting, is changed through `torqcli`, a shell driven like a
network device (candidate, `show | compare`, `commit`, `commit confirmed`, `rollback`). A small
Rust broker is the only writer of the machine's config repo: it applies each change as a git
commit that becomes a NixOS generation, records every step in a tamper-evident ledger, and can
undo a change in one command (a later change to the same files is undone first). AI layers come later, and are planned for from the start: an
AI will only ever propose changes, never change the system directly.

- Start here: [`docs/VISION.md`](docs/VISION.md) → [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- The shell: [`docs/CLI-DESIGN.md`](docs/CLI-DESIGN.md) · the tree: [`docs/CONFIG-IS-THE-DEVICE.md`](docs/CONFIG-IS-THE-DEVICE.md)
- Security: [`docs/SECURITY-MODEL.md`](docs/SECURITY-MODEL.md) · AI layer (later): [`docs/AI-LAYER.md`](docs/AI-LAYER.md)
- Getting a dev VM running: [`docs/DEV-SETUP.md`](docs/DEV-SETUP.md)
- What's next: [`docs/ROADMAP.md`](docs/ROADMAP.md) · Why: [`docs/DECISIONS.md`](docs/DECISIONS.md)
- Working with Claude Code: [`CLAUDE.md`](CLAUDE.md)

Status: M1 (the trust loop) done; M1.5 (`torqcli`) built and live on the dev VM.
