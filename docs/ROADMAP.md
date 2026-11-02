# Roadmap

## Virtual machines (the dev VM on an M4 Mac; NixOS VM tests)

### M1 — The trust loop  ✓ done
In an ARM NixOS VM (UTM on the M4 Mac):

- [x] `broker/Cargo.lock` generated; `cargo test` green.
- [x] UTM VM switches to `/var/lib/torq/config#torq-utm` (from `templates/config`).
- [x] `torq-broker-init` turns the config dir into a git repo + ledger on boot.
- [x] `torq-broker propose` accepts a diff + reason, shows it.
- [x] `torq-broker apply` commits (with `Ledger-Entry` trailer), rebuilds, appends ledger.
- [x] `torq-broker rollback` reverts, rebuilds, appends ledger.
- [x] `torq-broker verify` detects an edited, added or removed field or entry (not yet: the newest
      entries cut off, until the ledger is anchored, M3; KNOWN-GAPS 2).
- [x] NixOS VM test `tests/trust-loop.nix` proves propose → apply → rollback.
  (Runs inside the UTM VM without `/dev/kvm`: QEMU falls back to TCG; ~75 s of test time.)

### M1.5 — CLI polish: the everyday commands feel finished  ← current
Target and acceptance criteria: `docs/CLI-DESIGN.md`.

**`torqcli`: the network-device model (ADR-016)** — replaces `torq pkg/service/update/log/undo/status`:
- [x] Broker: `propose --base`, `apply --confirm` (test activation, anchor), `confirm`, `expire-confirm`
      (transient timer, direct restore, boot check), pending-confirm interlock, `revert`, JSON-only
      user-tier files at `users/<name>/<leaf>.json`; VM tests. Done so far: `--base` (1c), `revert`
      (1d), user-tier JSON files, fixed settings + cleared environment for a root broker (1e),
      `apply --confirm` / `confirm` / `expire-confirm` with the timer, boot check and freeze (1f).
- [x] Schema-driven tree: `system packages/host-name/time-zone/cli`, `services`,
      `users <name> packages/cli`; `/etc/torq/schema.json` in the system; fixed output
      `nixosConfigurations.torq`. `system/host.json` (host-name, time-zone; 2b),
      `system/cli.json` + `users/<name>/cli.json` (CLI style, user tier; 2c; removed by ADR-021), the schema (2d), with
      an evaluation test (`tests/tree.nix`).
- [x] Environment variables: covered by the tree (S3, 2026-10-31): system-wide ones are NixOS's own
      `environment variables <NAME> <value>`, system tier, shown and approved like any setting (the
      fence keeps the broker's out; control characters are refused; no secrets: the repo is
      world-readable, KNOWN-GAPS 6r); a person's own are theirs, in their home. No design of their own.
      (Reopen if per-user variables delivered by TorqOS are wanted.)
- [x] `torqcli` shell: operational + configuration modes, J style, pipes, `?`, Tab, history
      (3a–3d; pasting: every line runs, a commit never takes part of a failed paste).
- [x] One command model (4a: round-trip tested; edit / up / top; `delete` as on a network device, ADR-022). The C style
      and `switch cli` were built here and removed by ADR-021 (2026-11-01): one syntax.
- [x] Filters after any `show`: the real Linux tools (`grep`, `wc`, `head`, `tail`, `sort`, `uniq`,
      `cut`, `tr`; ADR-017, 4b); shell-style quoting; `torqcli <command>` without `-c` (4c);
      `| display detail` and `| display diff` (4d).
- [x] Candidate per user with its base commit; `set` / `delete` on the tree; `rollback 0`.
- [x] `show configuration`, `show | compare`, `show system commit`, `show system rollback <n>
      [compare <m>]`, `show packages` (was `show packages available`), `show services`, `show system alarms` (3e).
- [x] `commit check` (evaluates the committed base; building is left to the commit); `commit
      [comment]`; `commit confirmed [<minutes>]`; system-tier commits (and others' folders) print
      the change and need a terminal. Not yet run end to end against a real broker: the cutover.
- [x] `rollback <n>`, `request system repair`; `request system update [all]` and `request system
      software rollback <n>` load the software (`flake.lock`) into the candidate (4e).
- [x] Configuration numbers from the ledger (4f); `request system revert <change>`, `show system
      ledger`, the SSH key check at commit; `torq pkg/service/update/log/undo/status` retired, each
      saying what replaced it (4g).
- [x] The tree is every NixOS setting, plus config files (S1–S3e, docs/CONFIG-IS-THE-DEVICE.md).
- [x] The rest of the machine in the tree: hardware and boot as NixOS paths, a fixed `flake.nix`,
      `system software torq source` (S4a, S4b); `save` / `load override|merge|set` (S5)
      (CONFIG-IS-THE-DEVICE.md 5–6).
- [x] ADR-025, torqcli on six single sources (2026-11-01): one grammar (`?`, Tab, abbreviations,
      refusals, `help`), each setting's rules from NixOS, one output path (a pager, `| no-more`),
      one message form, every command saying what it did, one place per setting (software and
      services at NixOS's paths, the catalog folded in); `show version`, `show system uptime`,
      `show interfaces`, `show system storage` (CLI-DESIGN.md). Acceptance still to do: the walk of
      every `?` and fresh usability reviews.
- [ ] Later: `commit at <time>`, `configure exclusive`.

**First round** (done with the earlier command names; carried over):
- [x] Broker `--json` for every command (shapes and exit codes in `CLI-DESIGN.md`).
- [x] The CLI uses it and stops parsing broker text (ADR-014).
- [x] Human output for every command: changes shown as packages/services with versions and
      descriptions, not diffs; one result line with the change number and how to undo it.
- [x] Quiet progress: phase + elapsed time; full log to `~/.local/state/torq/last.log`, shown
      on failure.
- [x] Errors say what, why and what to do; no `Caused by:`, git hints, hashes or store paths by
      default; `torqcli --verbose` (`-v`) shows each cause and where the whole log is. No torqcli
      `--json`: machine-readable output is the broker's (`torq-broker --json`, ADR-014), and the
      configuration as data is `| display set` and `save`.
- [x] Tier visible in every prompt: system changes marked and defaulting to No.
- [x] Colour with meaning; plain text for `NO_COLOR` and non-terminals; fits 80 columns.
- [x] Snapshot tests of every command's output: log, status, rows and errors; torqcli's session
      tests assert whole outputs (show, compare, commit questions, refusals). The old
      pkg/service/update/undo commands were retired (4g).
- [x] New, small: "did you mean" for misspelled packages; `torq status --repair` to return a
      drifted machine to its ledgered system (asks first; ADR-012 still: never automatic).
- [x] Terminal look: TorqOS's shell, zsh + oh-my-zsh with the TorqOS prompt (modules/shell.nix);
      torqcli in colour (`show | compare`, `show configuration`, its prompt, errors, `?`; paint.rs).
- [ ] Usability check: someone new to TorqOS installs, removes, undoes and reads the log unaided.

### M2 — Install: TorqOS as a ready image
Nobody has to build TorqOS to install it: a release is an image to download and boot. Updates work
as NixOS's do (ADR-023): almost everything comes signed from NixOS's own binary cache; the machine
builds TorqOS's few small pieces itself, in minutes — no service for the project to run. Ease of use
first.

- [ ] **qcow2 image** (aarch64 first, testable here; then x86_64): download, boot in any VM (UTM,
      QEMU, Proxmox), log in. The config repo and ledger are created on first boot. *aarch64 done:
      installed in UTM from the image (2026-11-02). x86_64 and aarch64 (qcow2, and a VMDK for ESXi)
      built by CI's `image` workflow, run by hand; to try on ESXi.*
- [ ] **First-boot setup**, like a network device's initial configuration dialog: host name,
      time zone, the owner's account and password, how to log in remotely (password, SSH key, or
      both), asked once at the console, committed as change #1. No default password ships;
      passwords live outside the config repo (proposed: SECURITY-MODEL.md, "Accounts and
      passwords"). *Done; tried on the first install (2026-11-02).*
- [ ] **Customer-safe defaults:** dev mode off, SSH as chosen at first boot (passwords protected
      against guessing), the /etc/nixos guard on, nothing
      installed that is not needed.
- [ ] **Releases** (`v0.1.0`, …) at one URL machines fetch from; `request system update` moves to
      the newest one. *Built (2026-11-02, ADR-028): signed commits in the public
      `netstacks/torqos-releases`, `stable` moved forward to each tested release; machines take it
      as git and check each commit's signature before loading it; an image machine's first update
      moves it there. Done once the first release is out and a machine has updated to it.*
- [ ] **CI (GitHub Actions):** *test* on every push and pull request (`nix flake check`: tree
      checks and the VM tests, with KVM); *(the `test` workflow, 2026-11-02, with the DCO
      sign-off check on pull requests)* *nixpkgs bump* weekly, as a pull request the tests run
      on; *release* on a `v*` tag: the images built on x86_64 and arm64 runners, checksums, a
      GitHub Release, the tagged source copied to the release repo and `stable` moved *(the
      `release` workflow, 2026-11-02; the key an Actions secret for now, ADR-028)*. The move to the
      next NixOS release (every six months) stays a person's.
- [ ] **Downloads checked:** each image published with a checksum signed by a TorqOS release key
      (plain files on the release page; checked with `ssh-keygen -Y verify`, nothing to install).
      *(Built with the release, 2026-11-02; INSTALL.md says how.)*
- [ ] **ISO installer** for real hardware: the same image and first-boot setup, plus disk choice.
- [ ] **VM tests:** the image boots on a blank VM, first-boot setup, a commit, a rollback, an update.
- [ ] **Designs to settle first** (audits of 2026-10-31; KNOWN-GAPS 6r): where first-boot passwords
      live, since the config repo is readable by every user (a hash there can be cracked offline);
      checksums or signatures for the qcow2 and ISO themselves. (Decided, ADR-025: "SSH keys only"
      is a default TorqOS applies however SSH is turned on; root never logging in over SSH is an
      invariant.)

## Later: the AI layers (not planned now; everything built keeps them possible)

### M2 — The agent sees
- Local model reachable (llama.cpp on the Mac host during dev).
- `torq-agent` with read-only MCP tools (logs, hardware, config.read).
- "Doctor" mode: diagnose and explain, propose nothing.

### M3 — The agent acts
- `config.propose` tool → broker; tiers enforced; taint tracking.
- Agent proposals shown and approved in torqcli (`show system ledger`, the commit prompt).
- A default-deny allow-list at the broker for what an agent may propose (KNOWN-GAPS 6m).
- Signed commits (broker key), ledger anchoring.

## Later (not planned now): boot security, and limits on root

- Owner-rooted boot security (own Secure Boot keys, TPM2 + PIN; SECURITY-MODEL.md), tested in
  emulation first (QEMU + OVMF + swtpm).
- Strict mode: the owner's `sudo` runs only TorqOS's commands, with a ledgered break-glass
  (SECURITY-MODEL.md, "Root, the owner, and an AI the owner runs").
- Snapshots of data (home folders, the ledger's state) before changes, so what a root process
  deletes can be restored.

## Far future

- **The config repo with a private git remote.** At install, TorqOS asks for a GitHub/GitLab (or
  other) account and permission to create a private repo, and uses it as the config repo's remote:
  the system's settings are a true git repo off the machine too. To settle before building it:
  push-only or also accepting changes from the remote (never bypassing local approval); where the
  credential lives so an AI can never use it.

Dropped with the laptop and desktop goals (ADR-019): the reference-laptop stages, the desktop
and UI work. They are in git history.
