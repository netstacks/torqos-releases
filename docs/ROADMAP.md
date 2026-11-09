# Roadmap

One base, two faces (ADR-034): the sysadmin's server as code, and a person's machine an AI can
drive. M1–M2 built the base and the server face. M3–M5 build what the laptop face needs — and
each of them serves the server face too.

## Virtual machines (the dev host `torq5`, x86_64 on ESXi since ADR-030; NixOS VM tests)

### M1 — The trust loop  ✓ done
Done in the aarch64 UTM VM on the M4 Mac, which was the dev host at the time (retired, ADR-030):

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

### M1.5 — CLI polish: the everyday commands feel finished  ✓ essentially done
Target and acceptance criteria: `docs/CLI-DESIGN.md`. torqcli is the agent's interface first
(ADR-034): what remains here is human polish, and it stays below M3–M5.

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
      `show interfaces`, `show system storage` (CLI-DESIGN.md).
- [ ] Human polish, below M3–M5: the walk of every `?`; a usability check by someone new to TorqOS
      (installs, removes, undoes and reads the log unaided). Done: `configure exclusive` (2026-11-07, cli/src/exclusive.rs).
- [x] `commit at <time>` (approved now, applied then as that person; one at a time; refused if stale;
      `clear system commit`; `tests/commit-at.nix`, CLI-DESIGN.md).

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

### M2 — Install: TorqOS as a ready image  ← finishing
Nobody has to build TorqOS to install it: a release is an image to download and boot. Updates work
as NixOS's do (ADR-023): almost everything comes signed from NixOS's own binary cache; the machine
builds TorqOS's few small pieces itself, in minutes — no service for the project to run. Ease of use
first.

- [x] **qcow2 image** (aarch64 first, testable here; then x86_64): download, boot in any VM (UTM,
      QEMU, Proxmox), log in. The config repo and ledger are created on first boot. *aarch64 done:
      installed in UTM from the image (2026-11-02). x86_64 and aarch64 (qcow2, and a VMDK for ESXi)
      built by CI (the `image` workflow, and with each release); the x86_64 VMDK booted on ESXi 8.0.3
      (torq5, 2026-11-02).*
- [x] **First-boot setup**, like a network device's initial configuration dialog: host name,
      time zone, the owner's account and password, how to log in remotely (password, SSH key, or
      both), asked once at the console, committed as change #1. No default password ships;
      passwords live outside the config repo (SECURITY-MODEL.md, "Accounts and
      passwords"). *Done; tried on the first install (2026-11-02).*
- [x] **Customer-safe defaults:** dev mode off, SSH as chosen at first boot (passwords protected
      against guessing), the /etc/nixos guard on, nothing
      installed that is not needed. *(2026-11-02: tests/ssh.nix checks SSH's protections, and found
      the guessing protection blind (KNOWN-GAPS 6za, fixed); fonts, icons, NixOS's installer tools
      and bcache's left out of a machine without a screen, ~45 MB.)*
- [x] **Releases** (`v0.1.0`, …) at one URL machines fetch from; `request system update` moves to
      the newest one. *Built (2026-11-02, ADR-028): signed commits in the public
      `netstacks/torqos-releases`, `stable` moved forward to each tested release; machines take it
      as git and check each commit's signature before loading it; an image machine's first update
      moves it there. v0.1.0 out (2026-11-02), checked from outside. Done: `torq5`, installed from
      the image, had its first update move its source from the image's own store path to
      `git+…/torqos-releases?ref=stable` as one ledgered change, and runs a release.*
- [x] **CI (GitHub Actions):** *test* on pull requests, by hand and in every release (not on every push since 2026-11-05: the dev host runs it before each merge) (`nix flake check`: tree
      checks and the VM tests, with KVM); *(the `test` workflow, 2026-11-02, with the DCO
      sign-off check on pull requests)* *nixpkgs bump* weekly, as a pull request the tests run
      on *(the `bump` workflow, 2026-11-02: one branch, `bump/nixpkgs`, replaced each week until
      merged)*; *release* on a `v*` tag: the images built on x86_64 and arm64 runners, checksums, a
      GitHub Release, the tagged source copied to the release repo and `stable` moved *(the
      `release` workflow, 2026-11-02; the key an Actions secret for now, ADR-028)*. The move to the
      next NixOS release (every six months) stays a person's.
- [x] **Downloads checked:** each image published with a checksum signed by a TorqOS release key
      (plain files on the release page; checked with `ssh-keygen -Y verify`, nothing to install).
      *(Built with the release, 2026-11-02; INSTALL.md says how.)*
- [x] **ISO installer** for real hardware: the same image and first-boot setup, plus disk choice.
      *(ADR-031, `tests/installer.nix`: USB stick → chosen disk → its first boot; 2026-11-03.)*
- [x] **VM tests:** the image boots on a blank VM, first-boot setup, a commit, a rollback, an update.
      *(tests/image.nix, 2026-11-02: boot to setup, setup on the console, change #1, the update to a
      signed release and the refusal of a forged one; commits and rollbacks in trust-loop.nix. The
      builds are prebuilt, as a VM test cannot build systems.)*
- [x] **The skeleton** (ADR-032): NixOS's tools for changing a machine gone, the broker building
      and switching with `nix` itself, a size budget CI enforces.
      *(checks.x86_64-linux.size, 2026-11-03: 1228 MB of 1280; the seven tools absent and the
      `nix`-only switch in trust-loop.nix; the image and installer tests boot through it.)*
- [x] **The EFI-partition test** (ADR-033): a machine's boot menu held to what any writer must do.
      *(tests/boot.nix, 2026-11-03: entries after a switch, the default, an older entry chosen at
      the menu, a commit confirmed rolled back by a power cut, the editor off.)*
- [ ] **Python out** (ADR-032/033): the root grown by systemd-repart instead of growpart; then the
      boot writer is the last thing needing Python. Lanzaboote is deferred to Secure Boot (ADR-033,
      amended 2026-11-03); the branch is deleted, its lessons in the ADR.
      *(2026-11-07, `work/w6`: the root grown by systemd-repart in the initrd, the record's partition
      matched first and never grown; the SSH greeting's fastfetch without its Python completions. Left:
      the boot writer, ~135 MB — a smaller Python under NixOS's script or TorqOS's own writer, each
      needing tests/boot.nix and a second reviewer (ADR-032, amended 2026-11-07). The installer and
      image VM tests are still to be run on it.)*
- [x] **Upstreams** (ADR-039): a source statement on each release; CI fails an image or ISO that
      holds anything nixpkgs marks unfree (redistributable firmware, which it counts as free, is on
      the ISO). *(checks.<system>.free, 2026-11-03: nothing lets an unfree package into either, on
      both platforms, and each of eight ways planted in the image is caught; release.yml's notes.)*
- [ ] **Designs settled** (audits of 2026-10-31; KNOWN-GAPS 6r): where first-boot passwords
      live, since the config repo is readable by every user (decided, SECURITY-MODEL: outside the
      repo); checksums or signatures for the qcow2 and ISO themselves (done, ADR-028). (Decided,
      ADR-025: "SSH keys only" is a default TorqOS applies however SSH is turned on; root never
      logging in over SSH is an invariant.)

### M3 — Trust: the account, the door, the record (ADR-035, ADR-036)  ← next
Before an agent exists it needs an identity the kernel vouches for, and before a machine is a URL
its record has to live off the machine. Both faces want this: a server's ledger anchored in git is
the sysadmin's audit trail; a laptop's is what makes the agent's access safe.

- [x] **The broker as a socket service** (ADR-040, `tests/socket.nix`; on `main` 2026-11-05), identity from `SO_PEERCRED`; `torqcli` without sudo;
      `--actor` and `--approved-by` no longer taken from the caller (KNOWN-GAPS 4, 6a). System-tier
      approval asks the owner's password (a terminal now; a dialog on the desktop, M4). `torq-broker`
      under sudo stays for dev mode and recovery. The audit's socket items (N16, N15) with it.
- [x] **The agent's account** (built 2026-11-05, ADR-041, `tests/agent.nix`; Claude Code run as it with `torq agent run`, `tests/agent-run.nix`): `torq-agent`, unprivileged, sandboxed, no sudo, no shell; what it may
      propose a default-deny allow-list at the broker, by identity (KNOWN-GAPS 6m); config files
      refused to it outright.
- [x] **The config repo's remote** (ADR-036; push, anchor and pull built 2026-11-05, `tests/remote.nix`): `torq remote …` settings; push after every applied
      change, the ledger as `refs/torq/ledger`; `verify` against the remote's head (KNOWN-GAPS 2);
      `show system remote`, `request system pull` as a proposal into the candidate; the credential
      in the broker's state, scoped to one repo. Default stays local (ADR-004).
- [x] **Adopt at setup** (ADR-036, `tests/adopt.nix`; trust by the deploy key, signing later): "a fresh machine, or from a repository?" — the owner's key enrolled,
      HEAD's signature checked, this machine's hardware file kept, the owner's password asked,
      change #1. A VM test: a machine rebuilt from another's repo is that machine.
- [x] **Strict mode and `torq break-glass`** (ADR-035): sudoers limited to TorqOS's commands; a root
      shell only after a ledger entry, anchored. A VM test that a shell leaves its entry.
- [x] **Data snapshots before each system change** (ADR-038) on btrfs, in new images; restored by a person, with `request system snapshot restore`. The
      backup is built too (2026-11-07, under M4's data item); offering data at `rollback` is not.
- [x] Ledger hardening carried over: N3 (a torn append), M3 (password change in one transaction),
      the `Ledger-Entry` trailer cross-check.

### M3.5 — The install: one console UI, real hardware, recipes (ADR-046, ADR-047, ADR-048)
Before M4: it serves CLI and desktop installs alike. The owner's laptop (HARDWARE.md) is the target.

- [ ] **Real hardware** (ADR-046): the installer's hardware file turns on redistributable firmware and,
      where it finds a wireless interface, iwd; hardware profiles chosen by DMI (`hardware/`), the
      Zenbook S 16 first, split base / desktop. A VM test of the file written for a fake DMI and Wi-Fi.
- [x] **No network from stick to login** (ADR-048 5; built 2026-11-09, `tests/installer.nix`): the ISO carries
      what a first boot builds from (`hosts/installer/first-build.nix`: 362 paths, ~64 MiB beyond its store);
      `torq install` copies what the disk lacks, setup registers it; a VM with no network card builds
      change #1. x86_64 only (KNOWN-GAPS 6zu).
- [ ] **The installer's ways** (ADR-048 1–4, 6): the image's parts beside the whole; erase, alongside
      (NTFS, ext4, btrfs shrunk), free space, a partition; new UUIDs every install, file systems by UUID;
      systemd-boot on the existing ESP with TorqOS's own XBOOTLDR, other systems in the menu. VM tests
      on a disk holding other systems, each refusal a test.
- [ ] **The console UI** (ADR-047 1): `ratatui` over `crossterm` in torqcli, 80×25 and serial; the
      installer's disk steps, setup's network (Ethernet, or Wi-Fi through iwd), identity, SSH and
      starting point, a summary, the build's progress. The line-at-a-time setup kept as the fallback.
- [ ] **The answers file** (`torq-setup.json`): an unattended install; the VM tests install through it.
- [ ] **Recipes** (ADR-047 2–4): `recipe.json`; setup's starting point (TorqOS only, TorqOS Desktop,
      a recipe at a URL, my own repository); `request system recipe <url>`, `… recipe update`,
      `torq recipe publish`, `torq recipe list`; what is never taken refused (SSH keys, remote, users,
      caches' keys …). VM tests for each refusal, a recipe reverting itself, an update shown not applied.
- [x] **TorqOS installed on the laptop itself** (2026-11-09, three rounds, erasing its disk): CLI, offline first build, Wi-Fi joined in
      setup, the automatic restart; GPU, Wi-Fi, Bluetooth, the speakers' ASUS tuning up (HARDWARE.md). TorqOS Desktop next (M4).
- [x] **Setup, network first** (ADR-048 amended; built 2026-11-09): the network step (a cable, or Wi-Fi brought up on the
      image's boot from the carried firmware and joined with iwd), the repository offered only online, the restart
      on real hardware from the broker's unit, the failure's cause on screen, the version shown.

### M4 — The desktop as a configuration (ADR-034, ADR-037, ADR-038, ADR-045)
Not a product: a config repo, `torqos-desktop`, loaded on the skeleton. COSMIC, and only COSMIC
(ADR-045). Needs `torq-wire` (ADR-044) first.

- [ ] **`torqos-desktop`**: COSMIC and cosmic-greeter through NixOS's modules, its portal, Flatpak on;
      `modules/desktop.nix` rewritten as its module set (niri, tuigreet, Quickshell, foot, fuzzel go).
      Loaded with `load override` on an image machine, or adopted at setup (M3). A VM test that it
      boots to a COSMIC session; smoothness judged on real hardware (.37).
- [ ] **TorqOS Settings** (`settings/`, libcosmic; ADR-045 2): the tree, the candidate, commit,
      history, rollback, pending proposals, alarms, backups — a client of the broker's socket only,
      its words the broker's. A VM test that each action it offers is the torqcli request it names.
- [ ] **COSMIC Settings wrapped** (ADR-045 3): the pages that change what the tree owns (time zone,
      host name, users, …) hand the change to TorqOS Settings as a proposal; a patch in the overlay.
- [ ] **The person's COSMIC settings** (ADR-045 4): tree defaults, drift in `show | compare`,
      `request users <name> desktop save`; the tree's COSMIC words generated from cosmic-config's keys.
- [ ] **Applications by rung** (ADR-037): `users <name> flatpaks` reconciled at switch (only what it names); the COSMIC Store per user, its installs shown in `show | compare` and kept with `… flatpaks keep` (ADR-037 amended); the design
      for a person's own builds in the tree (`users <name> projects`), user tier; what TorqOS refuses
      (`curl | sh`, self-updaters) said where it can see it.
- [ ] **Data declared** (ADR-038): `services syncthing` and a sync mount as tree settings; the
      backup (`services restic backups`) with its key outside the repo and `request system restore`
      (built 2026-11-07: `request system backup key`, `show system backups`, `tests/backup.nix`; KNOWN-GAPS 6zm);
      the restore step in setup's from-a-repository path.
- [ ] **Approval as a dialog** (ADR-045 5): a system-tier commit on a desktop asks through polkit and
      COSMIC's agent (`org.torqos.approve`, `auth_self`), with the person's password; the same prompt
      text torqcli prints. Its two open questions settled first.
- [ ] **Releases carry the desktop configuration** as a second artifact, versioned with TorqOS.

### M5 — The agent (AI-LAYER.md)
On the account M3 made, over the configuration M4 made. The three scenarios of ADR-034 are the
acceptance tests, each a VM test.

- [x] **Doctor mode** (built 2026-11-07 on `work/w3`, ADR-042; `agent/`, `tests/doctor.nix` written,
      not yet run): `torq doctor "why is my wifi flaky?"` — `torq-doctor` as `torq-agent`, read-only
      tools only (the journal, hardware, `show configuration`, the ledger, alarms, NixOS option docs);
      explains, proposes nothing; the session tainted once it reads untrusted text. Local model by
      default (ADR-005, llama-server on a unix socket); a remote one as a setting.
- [x] **Proposals**, non-GUI (built 2026-11-07 on `work/w5`, ADR-042 6 as amended; `tests/agent-proposals.nix`
      written, not yet run): `torq agent propose "<ask>"` — the doctor with `propose_change`, structured
      words of the tree, through torqcli as the agent, which proposes and stops; the allow-list enforced
      at the broker; the taint held by the broker (sessions, `broker/src/session.rs`), sessionless
      proposals tainted; `request system apply` says why one is tainted; an agent's change to the
      network, SSH, units, /etc, boot, hardware, the console or the display applied only as a `commit confirmed`. Left: the
      desktop dialog (M4); a VM test of the doctor's own commit (KNOWN-GAPS 6zq).
- [x] **Session trust**, non-GUI (built 2026-11-07 on `work/w5`, ADR-042 7 as amended): `request system
      agent trust <minutes>` (at most 60, the person's password, their own folder) and `… revoke`;
      ledgered (grant, each apply under it, revoke, expiry), shown in `show system alarms`, never system
      tier, never tainted.
- [ ] **Build me an application**: the agent writes and builds in the person's home, installs via
      rung 3 (ADR-037), tests, undoes.
- [ ] **Watch the desktop**: screenshot and input through COSMIC's portal (ADR-045 6), granted per
      session and ledgered; remote input checked first. In the person's session; the agent's
      tooling, not the broker's.

## After 0.3.0: first in the next cycle

- [ ] **One vocabulary for the socket and the ledger** (ADR-044, KNOWN-GAPS 6zp): the crate `torq-wire`
      (`Request`, `Frame`, the exit codes; `Entry`, `Kind`, `Tier`, `Actor` with an `Unknown` case;
      `CommitId`, `StorePath`, `SnapshotLabel`) used by broker, torqcli and the agent. Fixture ledgers
      from today's broker first, each round-tripping byte for byte; then the socket's types, then the
      kinds, then the newtypes. Typed `data` per kind and splitting `broker/src/lib.rs` after that.

## Later (not planned now): boot security, and limits on root

- Owner-rooted boot security (own Secure Boot keys, TPM2 + PIN; SECURITY-MODEL.md), tested in
  emulation first (QEMU + OVMF + swtpm). Lanzaboote comes with it (ADR-033, deferred to here; its amendment holds what the first try learned).
- Strict mode without break-glass: no root shell at all, once every task an owner needs is a
  torqcli command (ADR-035).
- A shared base configuration with per-host overlays, for fleets (ADR-036).
- Mirrors of nixpkgs, a caching proxy that also prebuilds TorqOS's pieces, a source mirror —
  when there are users to serve (ADR-039).

Dropped with ADR-019 and not brought back by ADR-034: the reference-laptop stages, touch-first
design, the smoothness targets. They are in git history.
