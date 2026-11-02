# Architecture

Three layers, bottom up. Each lists what we **reuse** and what we **build**.

```
┌──────────────────────────────────────────────────────────────┐
│ 2  AI (later)      agent (unprivileged) ─► broker             │
│                    read tools · proposals only · ledger       │
├──────────────────────────────────────────────────────────────┤
│ 1  torq            torqcli (the configuration tree, the shell)│
│                    torq-broker (the only writer) · ledger     │
├──────────────────────────────────────────────────────────────┤
│ 0  Base            NixOS flake · the config git repo ·        │
│                    generations · a thin CLI-only system       │
└──────────────────────────────────────────────────────────────┘
```

## Layer 0: Base

- The **system configuration lives in a local git repo** at `/var/lib/torq/config`
  (a flake that imports the TorqOS modules, plus the machine's settings files).
- Every rebuild is a NixOS generation, listed in the boot menu.
- CLI only: no display server, desktop or graphical apps by default.
- **Reuse:** NixOS, nixpkgs. **Build:** the distribution modules (`modules/`).

## Layer 1: torq, the configuration part of the OS

- **`torqcli`**, a shell driven like a network device: operational and configuration modes,
  one syntax (the J style, ADR-021), a candidate, `show | compare`, `commit check`, `commit confirmed`,
  `rollback <n>`. Its tree is every NixOS setting, generated from the system's own options
  (docs/CONFIG-IS-THE-DEVICE.md), plus plain Nix config files for what is not a value.
- **`torq-broker`** (Rust, privileged, small): the only writer of the config repo; enforces
  tiers (user / system / forbidden); commits, rebuilds, confirms, rolls back; appends to the ledger.
- **Ledger**: append-only, hash-chained JSONL of every proposal and change.
- Filters (`| grep`, `| wc`) are the real Linux tools, sandboxed (ADR-017).

## Layer 2: AI (later; see `AI-LAYER.md`)

Not built now. Layers 0 and 1 are built so it can be added without a bypass: the broker is the
only writer, every proposal names its actor, and tiers decide what needs a person. The design, for then:

- **Agent** (unprivileged, sandboxed): reads the system through typed tools, *proposes* changes.
- Proposals go to the broker like anyone's; what an agent may propose is a default-deny
  allow-list by identity (KNOWN-GAPS 6m). A person approves system changes at a terminal.

## Change flow

```
person (torqcli) / agent ──propose(diff, reason, base)──► broker
                                                  │ 1. classify tier; refuse forbidden paths
                                                  │ 2. a person approves (system tier)
                                                  │ 3. git commit (Ledger-Entry trailer)
                                                  │ 4. build and switch  ──► new generation
                                                  │ 5. ledger.append(result)
                                                  ▼
                                 show system ledger · show system commit
undo ──► request system revert <change> / rollback <n> → commit → rebuild → ledger
broken boot ──► pick the previous generation in the boot menu
```

## Later

- Owner-rooted boot security (SECURITY-MODEL.md).
- Far future: the config repo with a private git remote (VISION.md).
