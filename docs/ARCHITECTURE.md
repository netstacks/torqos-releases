# Architecture

Four layers, bottom up. Each lists what we **reuse** and what we **build**.

```
┌──────────────────────────────────────────────────────────────────┐
│ 3  AI (next)       torq-agent, an account of its own ─► broker    │
│                    read tools · proposals only · session trust    │
├──────────────────────────────────────────────────────────────────┤
│ 2  Configurations  a server's · torqos-desktop · yours            │
│                    config repos, with a remote: the machine is a  │
│                    URL · applications by rung · data declared     │
├──────────────────────────────────────────────────────────────────┤
│ 1  torq            torqcli (the configuration tree, the shell)    │
│                    torq-broker (the only writer) · ledger         │
├──────────────────────────────────────────────────────────────────┤
│ 0  Base            NixOS skeleton · the config git repo ·         │
│                    generations · nothing a configuration can add  │
└──────────────────────────────────────────────────────────────────┘
```

## Layer 0: Base

- The **system configuration lives in a git repo** at `/var/lib/torq/config` (a flake that imports
  the TorqOS modules, plus the machine's settings files); optionally with a remote (ADR-036).
- Every rebuild is a NixOS generation, listed in the boot menu.
- A skeleton (ADR-032): what TorqOS itself runs, and nothing a configuration can add. No display
  server, desktop or applications in the base.
- **Reuse:** NixOS, nixpkgs. **Build:** the distribution modules (`modules/`).

## Layer 1: torq, the configuration part of the OS

- **`torqcli`**, a shell driven like a network device: operational and configuration modes,
  one syntax (the J style, ADR-021), a candidate, `show | compare`, `commit check`, `commit confirmed`,
  `rollback <n>`. Its tree is every NixOS setting, generated from the system's own options
  (docs/CONFIG-IS-THE-DEVICE.md), plus plain Nix config files for what is not a value. One typed
  grammar, every command saying what it did (ADR-025): the agent's interface as much as a person's.
- **`torq-broker`** (Rust, privileged, small): the only writer of the config repo; enforces
  tiers (user / system / forbidden); commits, builds and switches, confirms, rolls back; appends to
  the ledger. Next (ADR-035): a socket service that takes its caller's identity from the kernel.
- **Ledger**: append-only, hash-chained JSONL of every proposal and change; its head anchored off
  the machine (ADR-035, ADR-036).
- Filters (`| grep`, `| wc`) are the real Linux tools, sandboxed (ADR-017).

## Layer 2: Configurations — the two faces (ADR-034)

A configuration is a config repo loaded on the base. The base does not know which face it wears.

- **A server's:** the machine as code, in git, driven by torqcli; rebuilt anywhere from its repo.
- **`torqos-desktop`:** the compositor (niri), a terminal, the agent, a few applications — a
  configuration like any other, with the same gate, ledger and rollback. `modules/desktop.nix` is
  its module set.
- **Applications** by rung (ADR-037): nixpkgs in the tree; Flatpak declared in the tree; a person's
  own builds referenced from the tree. **Data** declared (ADR-038): a sync service, a backup,
  snapshots before each commit. Secrets never in the repo (the fence).

## Layer 3: AI (next; see `AI-LAYER.md`)

Layers 0 and 1 were built so it can be added without a bypass: the broker is the only writer,
every proposal names its actor, tiers decide what needs a person. Now (ADR-034):

- **Agent** (`torq-agent`: its own account, unprivileged, sandboxed): reads the system through
  typed tools, *proposes* changes. Its identity is the kernel's word (`SO_PEERCRED`, ADR-035), never
  a flag.
- Proposals go to the broker like anyone's; what an agent may propose is a default-deny
  allow-list by identity (KNOWN-GAPS 6m). A person approves system changes — at a terminal, or on a
  desktop in a dialog — with their password, the one thing a program in their shell does not have.
- Doctor first (read, explain, propose nothing), then proposals, then session trust ("for an hour,
  user-tier changes without asking" — still ledgered, still reversible).

## Change flow

```
person (torqcli) / agent ──propose(diff, reason, base)──► broker
                                                  │ 1. classify tier; refuse forbidden paths
                                                  │ 2. a person approves (system tier)
                                                  │ 3. snapshot data (ADR-038); git commit (Ledger-Entry trailer)
                                                  │ 4. build and switch  ──► new generation
                                                  │ 5. ledger.append(result); push, anchor (ADR-036)
                                                  ▼
                                 show system ledger · show system commit
undo ──► request system revert <change> / rollback <n> → commit → rebuild → ledger
broken boot ──► pick the previous generation in the boot menu
a remote commit ──► show system remote → request system pull → the candidate → the person commits
```

## Later

- Owner-rooted boot security (SECURITY-MODEL.md); strict mode without break-glass (ADR-035).
- A shared base configuration with per-host overlays (ADR-036); mirrors and a cache (ADR-039).
