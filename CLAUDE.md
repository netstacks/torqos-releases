# CLAUDE.md — TorqOS

> **TorqOS** in prose, `torq` in code, commands and paths. Renamed from the placeholder "Kestrel"
> on 2026-10-30; GitHub: `netstacks/torqos`.

## What this project is

TorqOS is a **thin, CLI-only operating system** built on **NixOS**, where **the configuration is the
device** (ADR-019). Its goals:

1. **One configuration defines the machine.** Like a network device's config: `torqcli` edits one
   tree, every NixOS setting plus plain Nix config files, kept in the machine's config git repo
   (`docs/CONFIG-IS-THE-DEVICE.md`). torq is the small, tightly controlled configuration part of
   the OS; everything else is plain NixOS.
2. **Every change is controlled, auditable and reversible.** A small privileged **broker** is the
   only writer of the config repo: each change is a git commit that becomes a bootable NixOS
   generation, recorded in a tamper-evident ledger, undoable in one command.
3. **Ready for AI, later.** No AI work now, but everything is built so AI layers can be added
   later, limited permanently and without bypass: an AI will only *propose* configuration changes;
   people stay free (anything in their own home, any change they approve).

Not goals now: the AI layers (later), a desktop or any UI, laptop- or touch-specific design, hardware-specific work, a
broad "complete OS". Owner-rooted boot security is later; a private git remote for the config
repo is far future (`docs/ROADMAP.md`).

Read `docs/VISION.md` first, then `docs/ARCHITECTURE.md`.

## The one rule that shapes everything

**The AI (when it comes) never changes the running system directly.** It edits the system *configuration*
(the local git repo at `/var/lib/torq/config`), and the broker builds the system from it.
That is what makes auditing and reversal automatic. Do not add code paths that bypass this.

## Repository layout

```
flake.nix              entry point: VMs, ISO, checks, packages
modules/               the distribution, as NixOS modules (torq.* options)
  base.nix             nix settings, users, locale, core packages
  security.nix         hardening (boot chain and TPM: later)
  desktop.nix          a desktop (niri), off by default since ADR-019; kept to bring back later
  broker.nix           the privileged change broker service + config repo
  shell.nix            the login shell: zsh + oh-my-zsh and the TorqOS prompt (shell/torq.zsh-theme)
  ai.nix               local model runtime + agent service (a stub: the AI layers are later)
hosts/
  dev-vm/              throwaway VM (`.#nixosConfigurations.dev-vm-{aarch64,x86_64}`)
templates/config/      the per-machine config repo the broker manages
                       (`nix flake new -t .#config /var/lib/torq/config`)
broker/                Rust crate: torq-broker (ledger, policy, git, rebuild, rollback)
tests/                 NixOS VM tests
docs/                  design docs and decision log
```

Two repos, on purpose:
- **This repo** = the distribution (modules, broker source, docs). Developed normally, on GitHub.
- **`/var/lib/torq/config`** on each machine = that machine's config, a small flake that imports
  this repo via `torq.lib.mkSystem`. Local only. Written only by the broker.

## Commands

```bash
# Rust broker
cd broker && cargo test          # unit tests, incl. ledger tamper detection
cd broker && cargo run -- --help

# Nix (run on Linux or inside the NixOS VM; on macOS needs a linux-builder)
nix flake check                                   # eval + VM tests
nix build .#nixosConfigurations.dev-vm-aarch64.config.system.build.vm
./result/bin/run-torq-dev-vm                   # boot the throwaway VM (dev / torq)
nix build .#checks.aarch64-linux.trust-loop       # run the trust-loop VM test

# Inside the UTM VM, against the machine's config repo
sudo nixos-rebuild switch --flake /var/lib/torq/config#torq   # first time only
sudo torq-broker propose --diff change.diff --reason "..." --actor user
sudo torq-broker apply <seq> --approved-by $USER
sudo torq-broker revert <applied-seq> --approved-by $USER
sudo torq-broker log && sudo torq-broker verify
```

## Current milestone (M1.5) — `torqcli`: TorqOS driven like a network device

A shell with operational and configuration modes, one syntax (the J style, ADR-021), a
candidate configuration, `show | compare`, `commit check`, `commit confirmed`, `rollback <n>`, and a
configuration tree that is the config repo's files (ADR-016, `docs/CLI-DESIGN.md`). Built in small,
reviewed steps, broker first. Next: the rest of M1.5, then M2, Install: a qcow2 image, then an ISO (`docs/ROADMAP.md`); the AI layers are later.

## Engineering principles

- **Privileged code is Rust**, small, and boring. The broker must stay auditable line by line.
- **No `unsafe`, no dead code — the compiler forbids them** (ADR-020): `#![forbid(unsafe_code)]` and
  `#![forbid(dead_code)]` at every crate root. Never add `#[allow(dead_code)]`, `#[allow(unsafe_code)]`
  or an `unsafe` block: use a safe API (rustix, std), delete the dead code, or redesign. A derive
  macro that writes an `allow` (clap's) is not used.
- **CLI only.** No UI layer; the agent runs unprivileged and sandboxed.
- **Everything declarative.** If it isn't in the config repo, it doesn't exist after a rebuild.
- **Test the trust properties, not just features.** Every security or reversibility claim
  should have a NixOS VM test (`tests/`) that would fail if the claim broke.
- **Thin.** What the system does not need is not installed; torq stays small.
- **Prefer existing, maintained components** (NixOS, nixpkgs, the real Linux tools, llama.cpp,
  MCP). Original work goes into the broker, ledger, torqcli and the agent tools.

## Guardrails for Claude Code working in this repo

- Never weaken a security default to make something work. Add an explicit, documented option
  instead, defaulting to the secure value.
- Never give the agent a code path to: modify its own permissions, disable/edit the ledger,
  touch Secure Boot keys, or run arbitrary root commands. Those are user-only, done by a person at a terminal.
- Record significant design choices in `docs/DECISIONS.md` (short ADR entries).
- Keep `docs/` in sync when behaviour changes.
- Things in this scaffold marked `TODO(M1)` / `TODO(M2)` are the intended next steps.

## Current status (read this first) — 2026-10-30

- **M1 is complete** and running on the UTM VM: propose → apply → rebuild → ledger → one-command
  rollback; broken changes reset *and rebuilt* (ADR-008); a broker killed mid-change is reconciled
  on the next operation (ADR-010); `torq-broker status` reports any drift from the ledger.
  `tests/trust-loop.nix` passes inside the UTM VM (no `/dev/kvm`: QEMU falls back to TCG, ~2.5 min).
  Broker: 40 tests green (`cargo test`, also run in the Nix build sandbox).
- **Packages, services, updates, history** (ADR-011–013) are torqcli commands now (4g): the earlier
  `torq pkg/service/update/log/undo/status` are gone and say what replaced each; `torq` is torqcli.
  Drift is checked at boot and every 15 min.
- **A stray `nixos-rebuild` (the default `/etc/nixos`) cannot switch the machine away** from its
  ledgered config: `/etc/nixos` holds stubs that refuse (`torq.broker.guardEtcNixos`). This happened
  once from another tool on this VM. Root can still switch any other way (`--flake /tmp/x`, a
  built system's `switch-to-configuration`): that is drift, reported afterwards, not prevented.
  Never build `/etc/nixos` or any config other than the broker's.
- **Way of working:** small steps; an independent reviewer agent behind every meaningful change;
  mutation-check new guards (break them, see a test fail); verify in the Nix sandbox and VM test.
- **M1.5 `torqcli` (ADR-016) is built and live:** one syntax (ADR-021), candidate, `commit check`,
  `commit confirmed` (timer, reboot rollback), `rollback <n>`, the tree (`system host-name /
  time-zone / packages`, `services`, `users <name> packages`) from
  `/etc/torq/schema.json`. On 2026-10-30 the VM was cut over to TorqOS (fresh ledger); there the
  first real `torqcli` commit, a confirmed commit, and an expired one rolling back all worked.
- **Filters and `torqcli <command>` (4b, 4c; ADR-017):** `show … | grep -i x | wc -l` runs the real
  coreutils/grep, sandboxed in bubblewrap (no files, network, environment or inherited descriptors);
  `torqcli show system alarms` runs one command. Covered in `tests/trust-loop.nix`.
- **The tree is every NixOS setting (S1–S5, docs/CONFIG-IS-THE-DEVICE.md):** `set virtualisation docker enable true`; old names are aliases; `file edit` + `set <place> config <file>` attach plain Nix files; `system software torq source`; `save` / `load override|merge|set`. `| display detail` / `| display diff` (4d); the software in the candidate (`request system update`,
  4e); configuration numbers from the ledger (4f); `request system revert`, `show system ledger`, the
  SSH key check at commit, and the old commands retired (4g). Next: the rest of M1.5 (`docs/ROADMAP.md`); what is
  left of M1.5 needs a person (usability check, terminal look) or its own design (environment
  variables). Open issues and VM quirks: `docs/KNOWN-GAPS.md`.
- **ADR-025 rebuilt torqcli on six single sources** (2026-11-01): one grammar (`cli/src/grammar.rs`:
  parsing, `?`, Tab, abbreviations, refusals, `help`), each setting's rules from NixOS, a pager,
  every command saying what it did, one place per setting (software at `environment systemPackages`
  / `users users <name> packages`, services NixOS's own with TorqOS's protections however they are
  on; the catalog folded in). Acceptance (a walk of every `?`, fresh usability reviews) still to do.

### You are probably running inside the dev VM
- Host `torq-utm`, user **`viper`** (the template no longer names a user; `torq user` in
  the config repo's `nixos/torq.json` is the one place, since S3d). `torq dev enable true` there: passwordless sudo, you have root (ADR-009).
- `~/torq` = this repo (working copy; push to `github.com/netstacks/torqos`, branch `main`).
- `/var/lib/torq/config` = the machine's config repo; its `torq` input is
  `path:/home/viper/torq`.
- **Ship a distribution change to the running VM through the broker**, so it is ledgered. Commit
  in `~/torq` first (the config repo's `torq` input is that working tree), then, in a terminal (tmux),
  with a **private candidate** (`-c`) — never the interactive one, which every interactive session of
  the user shares, and may hold a person's uncommitted edits:
  ```bash
  torqcli -c "request system update; configure; show | compare; commit comment \"…\""   # asks y/N
  ```
  Read the prompt before answering: it must show only `[edit system software]` (not `[edit system
  software torq]`, which would change where TorqOS comes from).
  To change the machine's config repo itself (not just bump inputs), do it by hand:
  ```bash
  rm -rf /tmp/cfg && cp -r /var/lib/torq/config /tmp/cfg && cd /tmp/cfg   # edit, then:
  git diff > /tmp/change.diff
  sudo torq-broker propose --diff /tmp/change.diff --reason "..." --actor user
  sudo torq-broker apply <seq> --approved-by viper
  sudo torq-broker status        # the machine is as the ledger says?
  ```
- Dev mode is a temporary exception for *building* TorqOS. Do not design the TorqOS agent around
  having root; the agent itself stays unprivileged and broker-only.
