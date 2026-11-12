# CLAUDE.md — TorqOS

> **TorqOS** in prose, `torq` in code, commands and paths. Renamed from the placeholder "Kestrel"
> on 2026-10-30; GitHub: `torqosorg/torqos`.

## What this project is

TorqOS is a **thin operating system** built on **NixOS**, where **the configuration is the
device** (ADR-019), with **one base and two faces** (ADR-034): the sysadmin's server as code in
git, and a person's desktop or laptop that an AI can drive safely. Its goals:

1. **One configuration defines the machine.** Like a network device's config: `torqcli` edits one
   tree, every NixOS setting plus plain Nix config files, kept in the machine's config git repo
   (`docs/CONFIG-IS-THE-DEVICE.md`). torq is the small, tightly controlled configuration part of
   the OS; everything else is plain NixOS. With a remote, the machine is a URL (ADR-036).
2. **Every change is controlled, auditable and reversible.** A small privileged **broker** is the
   only writer of the config repo: each change is a git commit that becomes a bootable NixOS
   generation, recorded in a tamper-evident ledger, undoable in one command.
3. **The AI fenced, people free — and next, not later.** The agent is an account the kernel
   vouches for (ADR-035), unprivileged; it only *proposes* configuration changes; people stay free
   (anything in their own home, any change they approve). The desktop is a *configuration*
   (`torqos-desktop`) loaded on the same base, not a product (ADR-034). torqcli is the agent's
   interface first, the administrator's second.

Not goals: laptop-*specific* or touch-first design, reference hardware, a smoothness target;
building a sync service or a cache of our own (ADR-038, ADR-039); preventing root (the agent
cannot get it; the owner can, not silently — ADR-035). Owner-rooted boot security is later
(`docs/ROADMAP.md`).

Read `docs/VISION.md` first, then `docs/ARCHITECTURE.md`. The owner's procedure for steering
sessions — what to build next, where a fresh session validates — is `docs/DRIVING.md`.

## The one rule that shapes everything

**The AI never changes the running system directly.** It edits the system *configuration*
(the local git repo at `/var/lib/torq/config`), and the broker builds the system from it.
That is what makes auditing and reversal automatic. Do not add code paths that bypass this.

## Repository layout

```
flake.nix              entry point: images, VMs, checks, packages, the dev shell
modules/               the distribution, as NixOS modules (torq.* options)
  base.nix             nix settings, users, locale, core packages
  security.nix         hardening (boot chain and TPM: later)
  desktop.nix          a desktop (COSMIC, ADR-045), off in the base; the torqos-desktop configuration's (ADR-034, M4)
  broker.nix           the privileged change broker service + config repo
  shell.nix            the login shell: zsh + oh-my-zsh and the TorqOS prompt (shell/torq.zsh-theme)
  setup.nix            first-boot setup, at the console of a machine made from the image
  installer.nix        the ISO installer's console: the image onto a chosen disk (ADR-031)
  releases.nix         where a machine takes TorqOS from, signatures checked (ADR-028)
  settings-fence.nix   what the configuration tree may never change
  dev.nix              development mode (ADR-009) — and the assertion that guards a path: source
  ai.nix               doctor mode and the model it asks: torq.agent.doctor, torq.agent.model (ADR-042)
  secrets.nix          torq-secrets: a program uses a secret and never reads it (ADR-052)
  agent.nix            the agent's account torq-agent, what it may propose, `torq agent run` (ADR-041)
hosts/
  image/               the release image's own system (`.#image`, `.#image-vmdk`)
  installer/           the ISO installer's system and the image it carries (`.#iso`)
  dev-vm/              throwaway VM (`.#nixosConfigurations.dev-vm-x86_64`; an aarch64 one is defined but nothing builds it)
templates/config/      the per-machine config repo the broker manages
                       (`nix flake new -t .#config /var/lib/torq/config`)
broker/                Rust crate: torq-broker (ledger, policy, git, rebuild, rollback)
cli/                   Rust crate: torqcli — the grammar, the candidate, the tree (ADR-025)
agent/                 Rust crate: torq-doctor — the agent: doctor mode, propose mode, its session at the broker (ADR-042)
wire/                  Rust crate: torq-wire — the socket's types and the ledger's words, shared by the three (ADR-044)
tests/                 NixOS VM tests
release/               allowed_signers: the keys a machine trusts a release from
.github/workflows/     test (pull requests, by hand, each release), weekly nixpkgs bump, release on a v* tag
branding/              the TorqOS logo, icons, terminal logos and palette (modules/branding.nix)
secrets/               Rust crate: torq-secrets, the secrets daemon (ADR-052; modules/secrets.nix)
scripts/               bump-version (the release) and bump-cosmic
settings/              Rust crate: torq-settings (TorqOS Settings, ADR-045 2), torq-applet (the panel, ADR-049) and torq-tunnel(s) (SSH tunnels, ADR-050), on torqcli's library
docs/                  design docs and decision log
```

Two repos, on purpose:
- **This repo** = the distribution (modules, broker source, docs). Developed normally, on GitHub.
- **`/var/lib/torq/config`** on each machine = that machine's config, a small flake that imports
  this repo via `torq.lib.mkSystem`. Local by default (an optional remote, ADR-036). Written only by the broker.

## Commands

The Rust toolchain is not installed in the system: it comes from the flake's dev shell, so the
machine stays thin. `nix develop` for a shell, or `nix develop --command …` for one command.

```bash
# The Rust crates
nix develop --command bash -c 'cd broker && cargo test'   # incl. ledger tamper detection
nix develop --command bash -c 'cd cli    && cargo test'   # the grammar, candidate, tree
nix develop --command bash -c 'cd agent  && cargo test'   # doctor mode, against a scripted model
nix develop --command bash -c 'cd wire   && cargo test'   # torq-wire: the socket's bytes (ledger fixtures: the broker's)

# Nix — x86_64 only: aarch64 is neither built nor tested during development (ADR-030, amended)
nix flake check                                   # eval + tree checks + VM tests
nix build .#checks.x86_64-linux.trust-loop        # one VM test
nix build .#checks.x86_64-linux.image             # boot, setup, a commit, a signed update
nix build .#image                                 # the qcow2 (.#image-vmdk for ESXi)
nix build .#iso                                   # the ISO installer, for real hardware (ADR-031)
nix build .#checks.x86_64-linux.installer         # USB stick → chosen disk → its first boot
nix build .#nixosConfigurations.dev-vm-x86_64.config.system.build.vm
./result/bin/run-torq-dev-vm                      # boot the throwaway VM (dev / torq)

# Against this machine's config repo (dev mode: sudo needs no password)
sudo torq-broker propose --diff change.diff --reason "..." --actor user
sudo torq-broker apply <seq> --approved-by $USER
sudo torq-broker revert <applied-seq> --approved-by $USER
sudo torq-broker log && sudo torq-broker verify && sudo torq-broker status
```

The VM tests need `/dev/kvm` to be quick; the check host has it (ESXi exposes nested
virtualisation). Without it QEMU falls back to TCG and they take minutes.

## Current milestone: 0.4.0-rc1 (in preparation, 2026-11-11)

Released so far: `v0.1.0` to `v0.3.2` (v0.3.2 on 2026-11-09). `0.4.0-rc1` is not released and no
version number has been bumped for it. Its goal is that the **desktop** (the COSMIC line, already on
`main`) and **secrets** (ADR-052 steps 1 to 3: a program uses a token or key and never reads it; on
`main` since `acc59e5`) both work. **Feature freeze until the tag**: only fixes, tests and documents.
The agent's further work (ADR-051, the resident agent, torq-mcp) is parked, by the owner's decision,
on `work/agents` (`20db80e`, tag `park/agents-known-good` at `eef8e57`), local to the owner's laptop
only and not on GitHub, until after rc1; do not build on it before then.

Milestones as built (`docs/ROADMAP.md`'s checkboxes are the most current record): M1, M1.5, M2 and M3
are done. M3.5 (the install on real hardware) is partly done: the offline first build, network-first
setup and the laptop install are built; the console UI, the answers file, recipes and the
installer's other ways are not. M4 (the desktop) is largely built. M5 (the agent) has its non-GUI half:
doctor mode, proposals and session trust.

## Engineering principles

- **Privileged code is Rust**, small, and boring. The broker must stay auditable line by line.
- **No `unsafe`, no dead code — the compiler forbids them** (ADR-020): `#![forbid(unsafe_code)]` and
  `#![forbid(dead_code)]` at every crate root. Never add `#[allow(dead_code)]`, `#[allow(unsafe_code)]`
  or an `unsafe` block: use a safe API (rustix, std), delete the dead code, or redesign. A derive
  macro that writes an `allow` (clap's) is not used.
- **The base is CLI only.** The one graphical piece, `settings/` (TorqOS Settings and the panel applets, desktop only), is a client of the broker's socket and has no privilege of its own. The agent runs unprivileged and sandboxed.
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

## Current status (read this first), 2026-11-11

- **What is released:** `v0.1.0` to `v0.3.2`, x86_64 only (images: qcow2, VMDK; the ISO installer
  since v0.2.0). No aarch64 downloads exist for 0.3.x, and nothing builds or tests aarch64.
- **What `main` holds beyond v0.3.2:** the desktop line, secrets and the docs pass, none of it through
  CI yet. The desktop: COSMIC through
  NixOS's modules, TorqOS Settings, a panel applet, an SSH-tunnels applet, approval through a polkit
  dialog (`modules/desktop.nix`, `settings/`, off by default). Setup now has a network/Wi-Fi step and
  a "new machine or from a repository" question. The desktop has run on exactly one physical
  machine, the owner's ASUS Zenbook S 16.
- **Secrets** (on `main` since `acc59e5`): `torq-secrets` (`secrets/`, `modules/secrets.nix`, ADR-052
  steps 1 to 3).
- **The agent is experimental.** The `torq-agent` account, `torq agent run`, doctor mode, proposals
  and session trust exist on `main`. Doctor is off by default, no model ships, and no real model has
  ever been tried (the tests use a scripted endpoint). The default allow-list is the owner's own
  packages. Anything beyond that is on `work/agents`, parked (above).
- **Tests.** Unit tests are `#[test]` functions: broker 250, torqcli 340, agent 28, secrets 12, wire
  11 (broker, torqcli and wire as `cargo test` in `nix develop` ran them on 2026-11-11). VM tests are `checks` in
  `flake.nix`, one file each in `tests/`. `tests/doctor.nix`, `tests/agent-proposals.nix` and
  `tests/commit-at.nix` ran green in CI in the v0.3.2 release run. CI's `test` workflow runs on pull
  requests, by hand and inside each release, **not on pushes to `main`**: the commits on `main` after
  v0.3.2 have not been through it, and the `desktop`, `tunnels` and `secrets` checks have not run
  in CI. Say what was actually run; do not write that something is tested when it only exists.
- **The dev host** is the owner's laptop (hostname `torqlaptop`, user `cwdavis`, repo `~/torq`), a
  TorqOS machine that is also the machine the desktop runs on. It is shared and throttled: heavy
  builds (`nix flake check`, VM tests, the images) go to the second machine, "the check host", an
  x86_64 ESXi VM with KVM. On the laptop use one job at a time. Session worktrees live beside
  `~/torq`; do not edit in another session's.
- **Known limits** are in `docs/KNOWN-GAPS.md`; the review of 2026-11-11 added three desktop and
  launcher items: 6zx and 6zy (the desktop save) fixed for 0.4.0-rc1 (follow-ups: 6zza to 6zzc), and 6zz (`torq agent run`'s
  set-id sweep) fixed too.

## Working here

- `commit check` evaluates the candidate; it does not build (`cli/src/grammar.rs`).
- **Way of working:** small steps; an independent reviewer behind every meaningful change;
  mutation-check new guards (break them, see a test fail); prove claims in the Nix sandbox and a VM test.
- **NixOS's own tools for changing a machine are gone** (ADR-032): the broker builds and switches with
  `nix` itself. Never build `/etc/nixos` or any configuration other than the broker's.
- **A `path:` input copies the working tree, uncommitted changes included**; it does not read git. A
  dirty `~/torq` becomes part of the system the broker ledgers: recorded and reversible, not
  reproducible. `git status` before shipping anything. Dev mode (`torq dev enable true`, ADR-009) and a
  `path:` source are two ledgered commits on top of a normal install (`docs/DEV-SETUP.md`).
- **Ship a distribution change to the running machine through the broker**, so it is ledgered. Commit
  first, then use a private candidate (`-c`), never the interactive one, which every interactive
  session of the user shares:
  ```bash
  torqcli -c "request system update; configure; show | compare; commit comment \"…\""   # asks y/N
  ```
  Read the prompt before answering: it must show only `[edit system software]`, not `[edit system
  software torq]`, which would change where TorqOS comes from. A system commit asks the owner's
  password at the terminal; it is never written into the repo or memory.
- Dev mode is a temporary exception for *building* TorqOS. Do not design the TorqOS agent around
  having root; the agent itself stays unprivileged and broker-only.
