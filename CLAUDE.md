# CLAUDE.md — TorqOS

> **TorqOS** in prose, `torq` in code, commands and paths. Renamed from the placeholder "Kestrel"
> on 2026-10-30; GitHub: `netstacks/torqos`.

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

**The AI (when it comes) never changes the running system directly.** It edits the system *configuration*
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
  agent.nix            the agent's account torq-agent, what it may propose, `torq agent run` (ADR-041)
hosts/
  image/               the release image's own system (`.#image`, `.#image-vmdk`)
  installer/           the ISO installer's system and the image it carries (`.#iso`)
  dev-vm/              throwaway VM (`.#nixosConfigurations.dev-vm-{aarch64,x86_64}`)
templates/config/      the per-machine config repo the broker manages
                       (`nix flake new -t .#config /var/lib/torq/config`)
broker/                Rust crate: torq-broker (ledger, policy, git, rebuild, rollback)
cli/                   Rust crate: torqcli — the grammar, the candidate, the tree (ADR-025)
agent/                 Rust crate: torq-doctor — the agent: doctor mode, propose mode, its session at the broker (ADR-042)
tests/                 NixOS VM tests
release/               allowed_signers: the keys a machine trusts a release from
.github/workflows/     test (pull requests, by hand, each release), weekly nixpkgs bump, release on a v* tag
branding/              the TorqOS logo, icons, terminal logos and palette (modules/branding.nix)
docs/                  design docs and decision log
```

Two repos, on purpose:
- **This repo** = the distribution (modules, broker source, docs). Developed normally, on GitHub.
- **`/var/lib/torq/config`** on each machine = that machine's config, a small flake that imports
  this repo via `torq.lib.mkSystem`. Local only. Written only by the broker.

## Commands

The Rust toolchain is not installed in the system: it comes from the flake's dev shell, so the
machine stays thin. `nix develop` for a shell, or `nix develop --command …` for one command.

```bash
# The Rust crates
nix develop --command bash -c 'cd broker && cargo test'   # incl. ledger tamper detection
nix develop --command bash -c 'cd cli    && cargo test'   # the grammar, candidate, tree
nix develop --command bash -c 'cd agent  && cargo test'   # doctor mode, against a scripted model

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

The VM tests need `/dev/kvm` to be quick; on `torq5` it is there (ESXi exposes nested
virtualisation). Without it QEMU falls back to TCG and they take minutes.

## Current milestone — 0.3.0: M3 finished, data, and the agent's non-GUI half

As of 2026-11-07 the owner set 0.3.0's scope: everything that is not desktop or GUI. M3's build list is done
(the socket broker, the agent's account and `torq agent run`, the remote with push, anchor, pull and adopt,
strict mode and break-glass, btrfs images with data snapshots, ledger hardening) and its audit
(`docs/AUDIT-2026-11-06.md`, in the repository since its findings were fixed or recorded, KNOWN-GAPS 6zl) was
worked through. Then: the release path (A2, A3), `commit at`, `configure exclusive`, systemd-repart;
the declared backup and `request system restore` (ADR-038); the agent's doctor mode, proposals with
taint tracking, and session trust (M5, AI-LAYER.md). The desktop (M4) and the agent's GUI parts come after.

### Earlier: M2 — Install: TorqOS as a ready image

Nobody has to build TorqOS to install it: a release is an image to download and boot. Done — the
qcow2 and the ESXi VMDK, first-boot setup at the console, customer-safe defaults, signed releases a
machine takes as git and checks the signature of (ADR-028), the three CI workflows, and the image
VM test. Updates work as NixOS's do (ADR-023): almost everything comes signed from NixOS's own
cache; the machine builds TorqOS's few small pieces itself.

The ISO installer (ADR-031) puts that same image on real hardware: `torq install` on its console
writes it to the disk a person names and leaves the detected hardware for first-boot setup.
Also built: a smaller TorqOS (ADR-032: no `nixos-rebuild` or NixOS's other tools on an installed
machine, a size budget), the EFI-partition test (ADR-033: Lanzaboote itself deferred to Secure
Boot), and ADR-039's source statement and unfree check (`checks.<system>.free`). The end-of-M2 audit is
done (`docs/AUDIT-2026-11-04.md`); **left of M2:** the `v0.2.0` tag.

**Then M3 — trust** (ADR-035, ADR-036): the broker as a socket service with identity from
`SO_PEERCRED`, the agent's account, the config repo's remote (push after every change, the ledger
anchored there, pull as a proposal, adopt at setup), strict mode and `torq break-glass`, data
snapshots. **M3.5 — the install** (ADR-046, ADR-047): a console UI for installer and setup, firmware
and Wi-Fi on real hardware (the owner's Zenbook S 16 is the target), recipes — anyone's shared
configuration repository as a starting point. **M4 — the desktop as a configuration** (COSMIC only,
ADR-045; `torqos-desktop`, the official recipe; applications by rung, ADR-037; data declared, ADR-038). **M5 — the agent** (doctor, proposals, session trust; the three
scenarios of ADR-034 as VM tests). `docs/ROADMAP.md` has each. What is left of M1.5 is human
polish, below these.

M1.5 built `torqcli` itself: operational and configuration modes, one syntax (the J style,
ADR-021), a candidate configuration, `show | compare`, `commit check`, `commit confirmed`,
`rollback <n>`, and a configuration tree that is the config repo's files (ADR-016,
`docs/CLI-DESIGN.md`), rebuilt on six single sources by ADR-025. Built in small, reviewed steps,
broker first.

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

## Current status (read this first) — 2026-11-04

- **Handoff (2026-11-04, M2 done; **v0.2.0 released** 2026-11-04, `stable` moved):** on `main`, each branch
  reviewed in a fresh session and its findings fixed: the ISO installer (ADR-031; KNOWN-GAPS 6zc);
  the skeleton (ADR-032; 6zd); the EFI-partition test (`tests/boot.nix`, check `boot`) —
  ADR-033's Lanzaboote **deferred** to Secure Boot (its premise was false: Python stays for
  growpart; branch deleted 2026-11-04, its reviews' findings in the ADR's amendment); ADR-039
  (`checks.<system>.free`, the release notes' source statement); the fence's holes (an /etc path
  with `..`, `specialisation`, `torq.installer`, /etc files other modules land in /etc/torq —
  modules/security.nix; 6ze); the SSH greeting and the README in the brand. The end-of-M2 audit
  (`docs/AUDIT-2026-11-04.md`): no high finding; A1 (gpg-signed releases accepted) and A4 (setup
  ledgering a failed switch) fixed, A3 and A11 in part, A2 and the rest in KNOWN-GAPS' audit table
  for M3. The version is 0.2.0 (`scripts/bump-version`, which with `--release` also tags and pushes); release.yml refuses a tag that is not the
  source's version, or would move `stable` back. torq5 runs this `main` (change #23, TorqOS 0.2.0).
  `nix flake check` green on 2026-11-04. **Next:** the owner's end-to-end ESXi install from the v0.2.0 ISO (CONTRIBUTING.md,
  "Releasing" for later ones); then M3, starting with the socket broker (A2 with it). The repo is public since 2026-11-04 (rulesets on `main` and `v*`, SECURITY.md).
- **Development moved to `torq5`** (ADR-030), an x86_64 ESXi VM with KVM, 64 GB and 1 TB; the
  aarch64 UTM VM on the M4 Mac is retired, and aarch64 waits for a stable x86_64 release. `docs/DEV-SETUP.md` is
  written for this host. Tests here: **broker 118, torqcli 241** (`cargo test` in `nix develop`,
  also run in the Nix build sandbox).
- **M2 is essentially done** — images (qcow2 + ESXi VMDK, both arches), first-boot setup,
  customer-safe defaults, signed releases (ADR-028; v0.1.0, v0.1.1 and v0.2.0 out), the test/bump/release
  workflows, and `tests/image.nix`. `torq5` was installed from the v0.1.1 image and its first
  update moved it to the releases repo, which was the last acceptance criterion for releases.
  The **ISO installer** (ADR-031, `tests/installer.nix`) is built: USB stick → chosen disk → its
  first boot. ADR-029 gave TorqOS its own `os-release`, boot menu and console
  greeting — committed but not yet in a release, so a machine on `stable` still says `NAME=NixOS`.
- **M1 is complete**: propose → apply → rebuild → ledger → one-command rollback; broken changes
  reset *and rebuilt* (ADR-008); a broker killed mid-change is reconciled
  on the next operation (ADR-010); `torq-broker status` reports any drift from the ledger.
  `tests/trust-loop.nix` passes on `torq5` with KVM: **376 s of test script**, ~12 min wall from
  cold including building the test system (2026-11-03). ROADMAP's M1 note of "~75 s" measured a far
  smaller test in 2026-10 — it is history, not a target.
- **Packages, services, updates, history** (ADR-011–013) are torqcli commands now (4g): the earlier
  `torq pkg/service/update/log/undo/status` are gone and say what replaced each; `torq` is torqcli.
  Drift is checked at boot and every 15 min.
- **NixOS's own tools for changing a machine are gone** (ADR-032): no `nixos-rebuild`, `nixos-option`,
  `nixos-version`; the broker builds and switches with `nix` itself (`torq-switch`). Building the
  default `/etc/nixos` with `nix` refuses: it holds stubs (`torq.broker.guardEtcNixos`), after
  another tool once switched this VM away. Root can still switch by hand (`nix build` of another
  flake, a built system's `switch-to-configuration`): that is drift, reported afterwards, not
  prevented. Never build `/etc/nixos` or any config other than the broker's.
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

### You are probably running on the dev host
- Host **`torq5`**, an x86_64 VM on a VMware ESXi server (ADR-030), user **`viper`** (the template
  no longer names a user; `torq user` in the config repo's `nixos/torq.json` is the one place,
  since S3d). 8 vCPU, 64 GB, 1 TB, nested virtualisation on — so `/dev/kvm` is there and the VM
  tests run at native speed. **aarch64 is set aside** until x86_64 is stable (no arm64 machine; not built in CI either), so
  assume nothing about it locally.
- It is a **normal install** — made from a release image (2026-11-02), then updated to the signed
  releases repo — with two ledgered, reversible commits on top, both applied on 2026-11-03:
  `torq dev enable true` (ADR-009: passwordless sudo, you have root) and `system software torq
  source path:/home/viper/torq`. `modules/dev.nix` asserts the second requires the first.
  `docs/DEV-SETUP.md` has both.
- **A `path:` input copies the working tree, uncommitted changes included** — it does not read git.
  So a dirty `~/torq` becomes part of the system the broker ledgers: recorded and reversible, but
  not reproducible from the repository. `git status` before shipping anything.
- `~/torq` = this repo (working copy; push to `github.com/netstacks/torqos`, branch `main`;
  commits are signed off under the DCO, `git commit -s`, and CI checks it).
- `/var/lib/torq/config` = the machine's config repo. Check which source it is on before assuming:
  `path:/home/viper/torq` is the dev state, `git+…/torqos-releases?ref=stable` the release state.
  While it is on a path, this machine is **not** exercising the release path real machines take, so
  a release still has to be checked from a machine tracking `stable` (ADR-028).
- The Rust toolchain is not in the system — it is the flake's dev shell (`nix develop`), so the
  machine stays thin and no toolchain change is ledgered.
- **Ship a distribution change to the running machine through the broker**, so it is ledgered. Commit
  in `~/torq` first (the config repo's `torq` input is that working tree), then, in a terminal (tmux),
  with a **private candidate** (`-c`) — never the interactive one, which every interactive session of
  the user shares, and may hold a person's uncommitted edits:
  ```bash
  torqcli -c "request system update; configure; show | compare; commit comment \"…\""   # asks y/N
  ```
  Read the prompt before answering: it must show only `[edit system software]` (not `[edit system
  software torq]`, which would change where TorqOS comes from). From ADR-040 on (the broker on a
  socket), torqcli needs no sudo, and a system commit then asks the owner's password at the
  terminal — the owner gives it for dev work; it is never written into the repo or memory.
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
