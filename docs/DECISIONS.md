# Decision log

Short ADRs. Newest at the bottom. Format: context → decision → consequences.

## ADR-001 — Build on NixOS, not Arch/Omarchy
- **Context:** Goals are auditable + reversible AI changes and ground-up security. Omarchy 4 is
  agent-first but Arch-based; recovery from bad agent edits is "reinstall configs".
- **Decision:** NixOS. The whole system is a declarative config; every change is a generation.
- **Consequences:** Reversibility by construction; steeper learning curve hidden behind UI/AI;
  non-FHS binaries need nix-ld/Flatpak/Distrobox. User count is not a goal — quality is.

## ADR-002 — Owner-only Secure Boot keys, no shim, Microsoft removed by default
- **Deferred by ADR-019** (later, not planned now).
- **Context:** Users must be able to secure the machine even from Microsoft; we don't want to be
  in the shim-signing business.
- **Decision:** Per-machine PK/KEK/db generated at install; option-ROM hashes from TPM event log;
  Microsoft keys optional (off). Lanzaboote signs UKIs locally.
- **Consequences:** One manual step (Setup Mode) at install; no Windows dual-boot by default;
  firmware updates must re-enroll option-ROM hashes; prefer iGPU hardware.

## ADR-003 — The AI only proposes config changes; a Rust broker applies them
- **Decision:** Agent is unprivileged. Broker is the sole writer of the config repo.
- **Consequences:** Every change is a commit + generation + ledger entry. No raw root shell for AI.

## ADR-004 — Local config git repo, never pushed by default
- **Decision:** `/var/lib/torq/config` is a local repo. Optional user-chosen encrypted backup.
- **Consequences:** Network is download-only (packages, updates). Updates are `flake.lock` commits.

## ADR-005 — Local models by default
- **Decision:** llama.cpp locally; cloud agents opt-in, indicated, ledgered.

## ADR-006 — Reference hardware
- **Superseded by ADR-019** (no reference hardware: TorqOS runs wherever NixOS runs).
- **Decision:** Dev on M4 Mac (ARM VM) → old ThinkPad (iGPU, TPM2) → 16" Strix Halo OLED.
  Apple Silicon only as a secondary dev profile (no owner-key boot chain there).

## ADR-007 — Privileged code in Rust, UI in QML
- **UI part superseded by ADR-019** (CLI only).
- **Decision:** Broker, ledger, tools in Rust. Shell in Quickshell (QML), unprivileged.

## ADR-008 — A failed rebuild is followed by a restore rebuild
- **Context:** `nixos-rebuild switch` can fail *after* activating the new system
  (switch-to-configuration exits 4 when a unit fails). Found in the UTM VM: the broker reset the
  repo but the running system kept the change, so system and config diverged.
- **Decision:** On a failed apply or rollback, the broker resets the repo to the previous commit
  *and rebuilds it*, then ledgers `apply_failed` / `rollback_failed` (since ADR-016:
  `revert_failed`) with `restored: true|false`.
  `torq-broker-init` runs only once (`ConditionPathExists=!…/.git`), never during a switch.
- **Consequences:** A failed change costs a second (usually cached) switch. If the restore also
  fails, the ledger says so explicitly rather than claiming the system is unchanged.

## ADR-009 — Development mode: Claude Code with full access, behind one switch
- **Context:** While TorqOS is being built, the fastest loop is Claude Code running *inside*
  the dev VM with root — editing `~/torq`, building with Nix, applying via the broker.
  Security during development is explicitly not a goal; being able to shut it off later is.
- **Decision:** `torq.dev.enable` (default **false**, `modules/dev.nix`): passwordless sudo
  for wheel, `claude-code` + `gh`, owner as trusted Nix user. Enabled only in the dev VM's
  config repo. This is a cloud agent with root, so it is a deliberate, temporary exception to
  ADR-003 (agent only proposes) and ADR-005 (local models by default) — for *building* TorqOS,
  not for the TorqOS agent itself.
- **Consequences:** Every rebuild prints a warning while it is on. Shutting AI access off is
  a single broker commit (`torq.dev.enable = false`), itself ledgered and undoable. The
  shipped product and all tests run with it off.

## ADR-010 — A broker that dies mid-operation is reconciled, never left half-done
- **Context:** apply/rollback commit *before* the minutes-long rebuild and ledger *after* it. A
  broker killed in between (OOM, Ctrl-C under sudo, power loss) left a commit nobody ledgered:
  the next apply failed ("does not apply"), rollback failed (empty commit), and an unaudited
  change stayed live. Killing the broker also does not stop `nixos-rebuild`, which may still
  switch the system after a retry has "restored" it.
- **Decision:**
  - The rebuild gets the broker lock as its stdin, so its whole process tree — including the
    `systemd-run --pipe` switch unit — holds the lock until the last process exits. git writes are
    fsynced (`core.fsync=all`, then `sync --file-system` on the repo).
  - apply/rollback record themselves in `state/pending.json` before committing, add the commit as
    soon as it exists, and remove the record once the outcome is ledgered.
  - The next apply/rollback, or `torq-broker reconcile --approved-by <name>`, finishes an
    interrupted operation: it keeps the unledgered commit (and any uncommitted changes) under
    `refs/torq/discarded/`, resets to the last ledgered commit, **always** rebuilds, and ledgers
    an `interrupted` entry. `propose` (the agent's entry point) and `init` never rebuild: they
    refuse until reconciled.
- **Consequences:** An interrupted change is never silently kept; it is discarded, recorded and
  can be re-applied. A reconcile costs one rebuild. Some states still need a human (see
  `KNOWN-GAPS.md`): a broken ledger, a HEAD that does not descend from the recorded commit, or
  leftover git lock files other than `index.lock`. Until the boot-time hook lands, a machine that
  lost power mid-switch runs whatever generation it booted until the next broker command.

## ADR-011 — Installed software is data in the config repo, not Nix code
- **Context:** Installing software is the most common change anyone makes, and the agent will make
  it too. Having tools edit Nix lists inside `.nix` files is fragile, and `users/<name>.nix` was
  user tier even though it is a full NixOS module (sudo rules, services: an escalation path).
- **Decision:** (Paths since ADR-016: `system/packages.json`, `users/<name>/packages.json`.) `packages.json` (system tier) and `users/<name>.packages.json` (user tier) are JSON
  lists of nixpkgs attribute paths, read by `modules/packages.nix` when the config flake passes
  `torq.lib.mkSystem { configDir = ./.; }`. User tier is that exact data-file shape and
  nothing else: any `.nix` file is system tier wherever it lives (`broker/src/policy.rs`). Names
  are a top-level attribute or one level under a known package set; unfree/broken/unsupported
  packages are refused at evaluation with a clear message. `torq pkg` (unprivileged) and later
  the agent's packages MCP server change them by proposing a diff through the broker.
- **Consequences:** A package change is a one-line JSON diff anyone can review, ledgered and
  undoable like any other. A user-tier change can only add nixpkgs packages to that user's profile
  (nothing is linked system-wide). GUI apps via Flatpak (ARCHITECTURE.md) remain a later layer.

## ADR-012 — Drift from the ledger is detected and reported, never silently corrected
- **Context:** Another tool switched the dev VM to a stock NixOS config outside the broker, and
  nothing noticed until things broke. The ledger said one system was running; another was.
- **Decision:** Every rebuild — and `init` — records the system it leaves running.
  `torq-broker status` compares the machine with its ledger: running system and next boot vs the
  last record, config repo HEAD and cleanliness vs the last ledgered commit, failed restores,
  interrupted operations, the chain. It runs under the broker lock (try-lock: mid-switch it reports
  `busy`, exit 75, and compares nothing); operations wait up to 3 s for the lock, so a check never
  fails them. A `torq-status` unit runs it at boot and every 15 minutes, writes problems to
  `/run/torq/drift` (printed by every new interactive shell) and to the journal at warning
  priority, and **never fails**: it can run during a switch, and a failed unit would fail the switch,
  which the broker treats as a failed rebuild and rolls back (ADR-008).
- **Consequences:** Drift is visible within 15 minutes, at boot and in every new shell, but nothing
  is changed automatically (the one exception: an unconfirmed `commit confirmed`, which the person
  consented to in advance, ADR-016): the person decides — `torq status --repair` (the broker's `repair`:
  rebuild the last ledgered configuration, refuse if the config repo itself was changed, record what
  had drifted, and say so if the switch did not take), or apply, rollback, reconcile. Garbage collection
  (`nix.gc`, weekly, generations older than 30 days; `min-free` during builds) is safe for the same
  reason undo works: a broker revert undoes the commit and *rebuilds*, it never needs an old
  generation on disk.

## ADR-013 — Services are a curated catalog, enabled as data
- **Context:** After software, the most common change is turning a service on or off (SSH,
  containers, VPN). Doing that meant hand-writing a NixOS diff, and the stock NixOS defaults are
  not always TorqOS's (SSH allows passwords and root by default).
- **Decision:** `services.json` in the config repo (system tier) lists names from a catalog in
  `modules/services.nix`; each entry is a service *with TorqOS's defaults* (SSH: keys only, no root
  login, only the owner; Podman: rootless, a `docker` command, no root API socket) and says what
  enabling it implies, because the approved diff is only `+ "name"`. The catalog's settings are
  asserted (a `mkForce` elsewhere fails the build), and SSH enabled outside the catalog with
  passwords is a build warning and is reported by `torq service list`. No `fwupd` yet: firmware
  must go through the broker. `torq service list|enable|disable` changes
  it through the broker like packages (ADR-011). (Since 4g, 2026-10-31: torqcli's `show services`
  reports it, and `set services <name>` / `delete services <name>` change it.) Unknown names are a type error listing the
  catalog. Enabling SSH lists the owner's keys (both `~/.ssh/authorized_keys` and
  `/etc/ssh/authorized_keys.d`) for the person approving, and refuses when there are none.
- **Consequences:** Common services are one confirmed, ledgered, undoable command. Anything outside
  the catalog is still a system-tier `.nix` change. The catalog grows deliberately, one reviewed
  entry at a time.

## ADR-014 — The broker speaks data; presentation lives in the CLI
- **Context:** The CLI prints the broker's output (git diffs, hashes, ledger kinds) and scrapes its
  text for numbers (`proposal #N`). That is both ugly and fragile, and making the broker "pretty"
  would grow the privileged code the project wants small and auditable.
- **Decision:** The broker keeps its plain text for people who run it directly and gains `--json`
  output for every command the CLI uses. All formatting, colour, progress, wording and help live in
  the unprivileged `torq` CLI (`docs/CLI-DESIGN.md`).
- **Consequences:** Polishing the experience never touches privileged code. The JSON shapes become
  an interface: changes to them are deliberate and tested on both sides.

## ADR-015 — The project is named TorqOS (code: `torq`); renamed from the placeholder "Kestrel"
- **Context:** "Kestrel" was a placeholder codename (and an existing nixpkgs package name).
- **Decision:** TorqOS in prose, `torq` for commands, code, options, paths and units, `TORQ_*` for
  environment variables; GitHub `netstacks/torqos`; the dev VM's host is `torq-utm`. A clean
  replace, no compatibility aliases: the dev VM was the only install.
- **Consequences:** The dev VM is cut over with a fresh config repo and a fresh ledger under
  `/var/lib/torq`; its old history is not migrated. Anything still saying Kestrel is either history
  (old ledger, git log) or a miss.

## ADR-016 — TorqOS is driven like a network device: `torqcli`, a candidate, commit confirmed
- **Context:** Commands applied each change immediately, and undo reverted one old change, which
  conflicts when later changes touched the same file. The owner is a network engineer; the network
  device model (a shell with operational and configuration modes, a candidate configuration,
  `show | compare`, `commit check`, `commit confirmed`, `rollback <n>`) is the best-tested answer to
  "change a live system safely".
- **Decision:**
  - `torqcli` is the shell (interactive, or one command per invocation for scripts). Two syntaxes
    over one model — J style and C style — switched with `switch cli`;
    the default is a setting. (Superseded by ADR-021: one syntax, the J style.) The earlier `torq pkg/service/update/log/undo/status` are removed.
  - The configuration is a tree whose containers are the config repo's folders and whose leaves are
    small data files; each setting is declared in a schema next to its NixOS module and installed
    with the system (help, completion, syntax); the modules' types, not the schema, are the authority.
    User tier is an explicit allow-list (`users/<name>/packages.json`; a leaf joins it only with the
    typed setting that reads it, e.g. `cli.json`), and the broker checks each leaf's JSON shape.
    Environment variables are held for their own design (values are effectively code). The broker
    always builds `nixosConfigurations.torq`, so changing the host name cannot break rebuilds.
  - Edits go into a per-user candidate that remembers its base commit; `commit` applies the change
    from base to candidate as one ledgered change, and the broker refuses it if the base is stale.
    A system-tier commit prints exactly what it commits and needs a terminal.
  - Commit numbers count backwards (0 = active). `rollback <n>` restores commit <n>'s
    configuration but never `flake.lock`; going back in software is the separate
    `request system software rollback <n>`.
  - `commit confirmed <minutes>` activates without changing the boot default; `commit` confirms
    (makes it the boot default, no rebuild); otherwise the broker restores the previous system
    directly from its store path, from a transient timer running the previous (known-good) broker;
    a reboot before confirming is itself the rollback. While one is pending, no other change can be
    made.
- **Consequences:** Most of this is unprivileged CLI work. The broker gains `propose --base`,
  `apply --confirm`, `confirm`, `expire-confirm`, the pending-confirm interlock and boot check, the
  ledger kind `confirmed`, allow-listed JSON-only user-tier files, a cleared environment with fixed
  paths and rebuild commands, and renames
  single-change undo to `revert`. Rollback is always a new change forward, so it cannot conflict.
  An automatic rollback of an unconfirmed commit is consented to in advance by `commit confirmed`
  and ledgered; it is not the silent drift correction ADR-012 rules out.

## ADR-017 — Output filters are the real Linux tools, not our own
- **Status:** accepted (2026-10-31)
- **Context:** Network devices filter `show` output with words of their own (`| match`/`| except`/`| count`, or `| include`/
  `| exclude`). TorqOS users are Linux users; the owner's call: "it should just work like any Linux
  CLI filtering", not a reimplementation of grep.
- **Decision:** After a `show` (and its own `| compare` / `| display set`), `| <tool> <args>` runs a
  real tool from a short allow-list of text filters (`cut grep head sort tail tr uniq wc`) with the
  output on its standard input and the arguments exactly as typed; filters chain left to right and
  only filters may follow a filter. A pipe filters output and must not be a way to run programs or
  touch files: torqcli is what scripts, and later the agent, are given, and the tools' own options
  can do both (`sort --compress-program=`, `sort -o`, `head ~/.ssh/…`; found in review). So each filter
  runs in bubblewrap — no network, no home, nothing but the read-only Nix store and an empty `/tmp`,
  an environment of the locale only — from a coreutils + gnugrep folder fixed at build time
  (`TORQ_FILTER_BIN`, `TORQ_BWRAP`, cli/package.nix), never from PATH; no descriptor torqcli was
  given reaches inside (close-on-exec past stderr; a second review's escape); 30 s, 64 MiB of output,
  a 64 MiB `/tmp` and at most 16 filters a line. A build without the sandbox (a Mac) runs no filter.
  Whatever a tool's option starts (`sort --compress-program=`) runs inside the same sandbox. The same words in both styles. The shell's word splitting follows a shell's: `'single'` quotes are literal,
  inside `"double"` quotes only `\"` and `\\` escape, and a quoted lone `"|"` is refused.
- **Consequences:** Every option works as in a terminal (`grep -Ei`, `sort -k2`, `uniq -c`) on the
  output, and the tools' own errors are shown; options that name files find none (`head /etc/passwd`:
  no such file). Unprivileged user namespaces must be available (NixOS: on by default). No
  `| no-more`: there is no pager. Results follow the tools, so `sort` order follows the locale. A
  `grep` that matches nothing is an empty answer, not an error (a script cannot tell them apart).
  An apostrophe now opens a quote, as in a shell: text with one goes in "double quotes".

## ADR-019 — TorqOS is a thin, CLI-only OS whose configuration is the device
- **Context:** The plans had grown to a complete laptop OS: owner-key boot on reference laptops, a
  touchpad-first desktop (niri, Quickshell), smoothness targets, Omarchy-level polish. What exists
  and works is the configuration part: torqcli, the broker, the ledger, the tree over every NixOS
  setting (2026-10-31).
- **Decision:** Focus on that alone. TorqOS is a thin, CLI-only, fully functional OS based on
  NixOS, with torq as the small, tightly controlled configuration part. Dropped from the goals
  and plans: laptop-specific design, touch- and touchpad-first, the desktop and every UI, the broad
  OS vision, reference hardware. Also no AI work for now: the AI layers (M2, M3) are later, and
  everything is built so they can be added without a bypass. Kept for later: owner-rooted boot
  security. Far future: the
  config repo with a private git remote (GitHub/GitLab account and repo created at install). The
  tree keeps NixOS's own paths, without a TorqOS shorthand for users, groups and applications.
- **Consequences:** VISION, README, ARCHITECTURE, ROADMAP, HARDWARE, SECURITY-MODEL, AI-LAYER
  and CLAUDE.md rewritten to match; earlier ADRs (002, 006, 007) stand as history, their laptop
  and UI parts superseded here. The desktop is kept, off by default (modules/desktop.nix), to bring back later; the dev VM
  turned it off with `set torq desktop enable false` (2026-10-31).

## ADR-020 — No unsafe code and no dead code: the compiler forbids them
- **Context:** An audit noted there was no crate-level guard against new `unsafe`, and that the
  broker's `take_over_process` ("call before any thread or child is started") was enforced only
  by call order. TorqOS is largely written by AI, so a rule nobody checks by hand must be one the
  compiler checks.
- **Decision:** Every crate root (`broker/src/lib.rs`, `broker/src/main.rs`,
  `broker/tests/trust_loop.rs`, `cli/src/main.rs`) has `#![forbid(unsafe_code)]` and
  `#![forbid(dead_code)]`: `forbid`, not `deny`, so no `#[allow]` below can undo it. (In a test
  build the harness itself marks `main` `allow(dead_code)`, so the binaries' test builds `deny`
  it; the real binaries, which the Nix build compiles, forbid it.) Every `unsafe` was replaced:
  `geteuid`, `umask`, `tcflush`, the filter's process-group kill through rustix's safe API; the
  broker no longer changes its own environment at all (no `clearenv`/`set_var`): every program
  it starts gets the fixed environment and `/` explicitly (`config::command`), which also removes
  the call-order invariant; the filter sandbox's inherited descriptors are closed by a bash step
  before bubblewrap (`exec {n}>&-`) instead of `close_range` in `pre_exec`. clap's derive
  macros write `allow(dead_code)`, so the broker's command line is declared with clap's builder.
- **Consequences:** New `unsafe` or dead code fails the build. libc is no longer a dependency;
  rustix (safe wrappers) is. The standard library and dependencies still use `unsafe` inside;
  this covers TorqOS's own code. The filter's process-group kill is kept as insurance for a race
  no test now reproduces.

## ADR-021 — One syntax: the J style
- **Context:** torqcli spoke two syntaxes over one command model (ADR-016): the J style (`set`,
  `delete`, `edit`, `show | compare`) and the C style (bare paths, `no`, `do`, `end`, `show
  running-config`), switched with `switch cli` and chosen by a setting (`system cli style`, `users
  <name> cli style`). Every help text, error, completion and test existed twice, and small
  differences between the two kept surfacing as oddities.
- **Decision:** One clean language: the J style. The C style is removed everywhere — its parsing,
  completion, prompt, `switch cli`, `--style`, the `cli style` settings and their files
  (`system/cli.json`, `users/<name>/cli.json`), `modules/cli.nix` and `/etc/torq/cli.json`, and the
  broker's `cli.json` user leaf. The command model (`Op`) stays: the session executes operations,
  and a round-trip test still checks every one parses back from how it is typed. Only `all` and
  `config` stay reserved as entry names (they mean something after a path).
- **Consequences:** `packages.json` is the only user-tier leaf. A leftover `users/<name>/cli.json`
  is refused at evaluation, like any file a person's folder may not hold, and the broker treats it
  as system tier. The other syntax's words (`no`, `do`, `end`, `switch`, `show running-config`) are
  unknown commands, never another meaning.

## ADR-022 — Pastes and deletes as on a network device
- **Context:** Testers found that a paste stopped at its first error (the rest silently not run) and
  that `delete <place>` needed `delete <place> all`; a network engineer pastes blocks of commands and
  expects every line to run, each error reported, and `delete` to take what it names.
- **Decision:** Every line of a paste runs; each error names its line. A `commit` while lines
  have failed since the person last looked at `show | compare` asks first; a question (`[y/N]`)
  takes only an answer typed after it, so a paste can never answer it and a commit never takes part
  of a paste unseen — no timing is part of that decision (a pause inside a paste must not make it
  look typed). A question ends what was pasted: the lines still waiting are shown as not run. `delete` takes what it
  names — one value, a setting, or everything below a place — without `all`; deleting what is not
  set is a warning. `delete` alone (everything at this level) asks first. Scripts (`-c`, piped
  input) still stop at their first error. Rules between settings are checked on the whole
  configuration (warned at once, refused at `commit`), never line by line.
- **Consequences:** `all` is no longer a reserved word (`config` still is). `show | compare` and
  `commit` remain where a person sees exactly what a delete takes before it happens.

## ADR-023 — Updates as NixOS does them; no service for the project to run
- **Context:** M2 planned a signed binary cache of every release so no machine ever compiles. The
  owner wants no service to run for users, and protecting TorqOS's source by shipping only binaries
  would not work anyway (built programs can be taken apart; a licence is what protects code).
- **Decision:** Updates work as NixOS's do: almost everything (kernel, services, applications) comes
  pre-built and signed from NixOS's own cache (cache.nixos.org); each machine builds TorqOS's own
  few small pieces (torqcli, the broker, its modules) itself, in minutes. Releases are plain files on
  a release page: the image and a checksum signed with a TorqOS release key (`ssh-keygen -Y`).
- **Consequences:** Nothing for the project to host but release files. An update takes a few
  minutes more than a pure download. A cache of TorqOS's own pieces can be added later without
  changing machines' design (a public key in their configuration), if that ever matters.
  (ADR-039: NixOS's cache is a service TorqOS depends on without an agreement; a caching proxy in
  front of it, where TorqOS's own pieces would be prebuilt, is the plan once there are users.)

## ADR-024 — The store survives a power cut
- **Context:** Cutting the power during first-boot setup's build (M2) left the Nix store broken: Nix
  by default records a store path as there before its files reach the disk (`fsync-store-paths =
  false`), and the cut left a recorded file empty. Every later build failed on it; setup could never
  finish. Any TorqOS machine losing power during a change could end up the same.
- **Decision:** `fsync-store-paths = true` on every TorqOS machine (modules/base.nix). Nix's caches
  in root's home are not covered by it (they are written without waiting either): setup clears them
  when it starts over; on a set-up machine they stay a known gap (KNOWN-GAPS 6t).
- **Consequences:** Builds that write many small paths are slower; a machine works after a power cut
  mid-change. Tried by cutting the power during setup in the image (docs/INSTALL.md).

## ADR-025 — torqcli is built on six single sources, not on fixes
- **Context:** Four usability reviews and a walk of every `?` (2026-11-01) found about 30 problems:
  `?` and Enter disagreeing, a false "unknown command", values accepted that fail at commit, no
  pager, text cut mid-word, errors echoing the line three times, commands silent about what they
  did, TorqOS's own settings hidden next to NixOS's. Each was a symptom of something torqcli
  described twice, or not at all. The owner: no band-aids or one-off fixes; a world-class CLI that
  acts and reacts as anybody would expect.
- **Decision:** torqcli rests on six single sources; every feature uses them, and each has a test
  over its whole surface rather than per case:
  1. **One command grammar** (`grammar.rs`): a declarative tree of the commands — words, help,
     values, the pipes each takes, and how a whole line becomes an `Op`. `?`, Tab, abbreviations,
     parsing, the explanation of a refused line, guidance for words from other CLIs (`| count` →
     `wc -l`), and `help` are all read from it. Configuration paths stay the tree's.
  2. **One value model:** each setting's rules (type, allowed values, format, range, read-only) from
     NixOS's own description of it. `?` shows the format, Tab completes closed sets, `set` refuses
     what commit would, and existing entries (users, interfaces) are offered.
  3. **One output path:** every table and notice is laid out by `ui.rs`; output taller than the
     window goes through one pager (`| no-more` skips it).
  4. **One message form:** the shell names the line once; a message says what is wrong, then what
     to do, in one quoting style.
  5. **Every command says what it did** (changes made or discarded, "no changes", nothing to show),
     and every way out of configuration mode (exit, Ctrl-D, the end of `-c`) reports uncommitted
     changes.
  6. **One place per setting:** TorqOS's own settings live at their NixOS paths, with TorqOS's
     protections applied there (SSH, Podman, Tailscale, printing: the catalog is folded in).
- **Consequences:** a larger rewrite of torqcli's grammar and settings layer now, instead of ~30
  patches; afterwards a new command or setting gets `?`, Tab, help, checks and errors by being
  declared once. Acceptance: the walk of every `?` and fresh usability reviews, before shipping.

## ADR-026 — The history is privileged
- **Context:** On the first install from the image (2026-11-02), out of dev mode, reading the
  history — `show system commit`, `show system ledger`, `show system alarms`, `rollback ?` — asked
  for the owner's `sudo` password: the ledger is root's (`/var/lib/torq/state`, 0700) and is read
  through the broker. Dev mode had hidden it (its sudo needs none). The options were a password-free
  rule for the broker's read-only commands, a copy of the ledger the owner can read, or keeping it.
- **Decision:** Keep it: the history is privileged. Reading it is the owner borrowing root through
  `sudo` with their own password, as changing the machine is (SECURITY-MODEL.md). Where sudo would
  ask and there is no terminal to ask at, torqcli says so before running anything (`broker()` in
  cli/src/main.rs), never "the broker stopped unexpectedly".
- **Consequences:** The first read of a session asks for the password; sudo's timestamp covers the
  rest for its usual few minutes. A script reads the history only with `sudo -v` first or at a
  terminal. An agent (M3) gets its own read access through the broker's socket, not this way.

## ADR-027 — Apache-2.0, every contribution signed off (DCO)
- **Context:** TorqOS is to be published as an open-source distribution; the repo had no licence
  (the crates said "MIT OR Apache-2.0", never decided), which leaves it all rights reserved. The
  owner's aim: as little legal exposure as possible, for the maintainer and for users.
- **Decision:** Apache License 2.0 (`LICENSE`, the text as apache.org publishes it): no warranty and
  no liability, as every open-source licence says, and also a patent licence from every contributor
  for what their contribution uses, ended for anyone who sues claiming TorqOS infringes a patent;
  contributions are under the same licence by default (section 5). Every contribution is signed off
  under the Developer Certificate of Origin 1.1 (`DCO`, `CONTRIBUTING.md`): a record that whoever
  gave it had the right to.
- **Consequences:** The crates and packages say Apache-2.0. Pull requests are checked for sign-offs
  (with CI). Owning the project through a company rather than a person is the owner's to arrange,
  outside the repo; none of this is legal advice.

## ADR-028 — Releases: signed commits in a public repo, checked by every machine
- **Context:** A machine installed from the image runs the TorqOS inside it, with nowhere to update
  from; development is in a private repo. Nix fetches what a flake names and checks its hash
  against the lock, but checks no signature: whoever controls where TorqOS comes from decides what
  every machine builds next, as root.
- **Decision:** Releases are made by CI (`.github/workflows/release.yml`) on a `v*` tag, after the
  tests and both images pass. Each is one commit of the tagged source in the public
  `netstacks/torqos-releases` (no development history, no workflows), signed with the TorqOS release
  key (SSH, ed25519), tagged; `stable` moves to it, forward only, and never for a pre-release
  (`v1.2.0-rc1`). The GitHub Release carries the images (the VMDK gzipped: release files are at most
  2 GiB), `SHA256SUMS`, its signature (`ssh-keygen -Y sign`, namespace `file`) and the key.
  Machines take TorqOS as git (`git+https://github.com/netstacks/torqos-releases?ref=stable`), so
  what is built is exactly the commit checked. Before a lock taking a releases commit enters the
  candidate (`request system update`, `set system software torq source`), torqcli fetches that
  commit and `git verify-commit` checks it against the key the machine was installed with
  (`/etc/torq/release-signers`, from `release/allowed_signers`); a commit the key did not sign is
  refused and nothing is loaded. A machine on the image's own TorqOS is moved to the releases by its
  first `request system update`. Where the releases are and their key are TorqOS settings
  (`torq.releases.url`, `torq.releases.signers`, modules/releases.nix; /etc/torq/releases.json),
  fenced from the tree: which key decides what every update installs is TorqOS's own.
- **Consequences:**
  - **The key.** Its private half is an Actions secret of the development repo for now
    (`TORQOS_RELEASE_KEY`; GitHub can use it, never show it); the owner keeps a copy, and moving it
    to a hardware key, with signing by hand, is later. A new key reaches machines as an update
    signed by the old one.
  - **Writing to the releases** takes a token that may write to that repo only
    (`TORQOS_RELEASES_TOKEN`, a fine-grained token: it expires, at most a year). Since
    2026-11-02 it may also push to this repo and open its pull requests, for the weekly nixpkgs
    bump (`.github/workflows/bump.yml`): what Actions' own token pushes starts no workflow, and
    the bump's tests must run.
  - **Not covered yet:** an attacker able to move `stable` back to an older signed release
    (a downgrade): `stable` only moves forward when CI moves it, and since the audit of
    2026-11-04 (A3) only to a higher version than it holds, but `show | compare` shows the release's
    date, not its version, and the machine does not refuse an older release by itself. The check
    is torqcli's, at update: a person who edits the lock by hand is past it (as they are past any
    check, being able to commit).

## ADR-029 — TorqOS names itself, as a NixOS derivative
- **Context:** A TorqOS machine said it was NixOS everywhere it names itself: `/etc/os-release`
  (`NAME=NixOS`, NixOS's version, URLs and logo), the boot menu, the console's greeting. The
  project now has its own identity (`branding/`, from the TorqOS brand kit).
- **Decision:** `modules/branding.nix` sets NixOS's own options for a derivative:
  `system.nixos.distroName = "TorqOS"`, `distroId = "torqos"` (so os-release says
  `ID_LIKE=nixos`), and os-release's values: TorqOS's version (torqcli's, one number) with the NixOS
  release under it (`VERSION="0.1.1 (NixOS 26.05)"`), the public repository's URLs, the logo's
  icon name and Torque orange (`ANSI_COLOR="38;5;208"`). NixOS's build id and its release's end of
  support stay. The console's `/etc/issue` shows the wordmark and the version; setup shows the
  wordmark over its welcome; a fastfetch configuration is installed for whoever installs fastfetch;
  the icons are installed with a desktop. The shell's prompt and colours are unchanged.
  `torq.branding` is fenced from the tree.
- **Consequences:** Tools that ask "is this NixOS?" by `ID` see `torqos` and must read `ID_LIKE`;
  NixOS's own switch checks `/etc/NIXOS`, which every TorqOS machine has. The version in
  os-release is the TorqOS release a machine runs, so a machine updated to a release says so. The
  repository's files are Apache-2.0; the name and logo are not licensed as trademarks
  (branding/README.md).

## ADR-030 — Development moves to an x86_64 ESXi host; aarch64 is CI's
- **Context:** TorqOS was built in an aarch64 UTM VM on an M4 Mac (ADR-006's "dev on M4 Mac").
  That machine had no `/dev/kvm`, so every NixOS VM test ran under QEMU's TCG emulation (the trust
  loop ~2.5 min), it was limited to 8 GB and a 64 GB disk, it paused whenever the Mac slept — and
  the Mac is company property, which is the wrong place for the project to live. `torq5`, an
  x86_64 VM on a VMware ESXi server, was installed from the v0.1.0 release image on 2026-11-02 and
  has since been given 8 vCPU, 64 GB of memory, a 1 TB disk and nested virtualisation.
- **Decision:** `torq5` is the development machine for everything day to day: the broker and
  torqcli crates, `nix flake check`, the VM tests (now with KVM) and the images. The Mac VM is
  retired, not kept as a second host. **aarch64 is covered only by CI**, on GitHub's arm64 runners,
  which already build both architectures' images on every release and run the tests on every push.
  The dev machine is itself a normal release install, so every change to it goes through its own
  broker and ledger — with two deliberate exceptions, both one ledgered commit and both reversible:
  `torq dev enable true` (ADR-009) and `system software torq source path:/home/viper/torq`, which
  points the machine at the checkout instead of the signed releases repo. `modules/dev.nix` asserts
  that the second requires the first, so a machine cannot quietly build from a writable folder.
- **Consequences:** VM tests run at native speed, and the disk is no longer a reason to avoid
  building images locally. An arm64-only failure has no local reproduction: it has to be chased
  through CI, or on a borrowed arm64 machine (KNOWN-GAPS, "Dev host quirks"). While its source is a
  path, `torq5` is not testing the release path that real machines take, so a release must still be
  checked from a machine that tracks `stable` (ADR-028). A `path:` input also builds the checkout's
  working tree, uncommitted edits included (KNOWN-GAPS, "Dev mode"), so what the ledger records on
  the dev machine is reversible but not always reproducible from the repository. `docs/DEV-SETUP.md` is written for this
  host; the Mac/UTM instructions are in git history.
- **Amended (2026-11-05, the owner):** during development the tests run here, not on every push:
  `nix flake check` on torq5 before each merge to `main`. CI's `test` runs for pull requests, by
  hand, and as the first step of every release. And aarch64 is set aside: no images until x86_64
  has a stable release (the flake still defines it; image.yml's matrix has it commented out).

## ADR-031 — The ISO installer writes the image, and detects the hardware
- **Context:** M2's last piece is TorqOS on real hardware: an ISO to boot from a USB stick that
  puts TorqOS on a disk. TorqOS already has one install artifact, tested end to end (the image,
  `tests/image.nix`): a disk that grows to fill whatever it is written to and runs first-boot setup.
  What a real machine adds is a disk to choose, and hardware a VM does not have — network cards'
  firmware, CPU microcode, storage drivers at boot — which the image's `hardware-configuration.nix`
  (a VM's: `qemu-guest.nix`) does not name.
- **Decision:** The ISO is a small TorqOS live system whose console runs `torq install` and nothing
  else: nobody can log in, nothing listens — no network, and nothing on the machine's disks
  started (no LVM, no RAID). It carries the image itself — the qcow2 CI builds and tests, converted
  to raw by `qemu-img` and compressed, so every install, VM or metal, is the same disk bytes.
  `torq install`:
  1. checks its own copy of the image (its SHA-256) before offering any disk: a damaged stick or
     download is found with nothing written;
  2. lists the whole disks it could install to (`/sys/block`): not loop, RAM, optical,
     device-mapper or read-only devices, an eMMC's boot areas, an NVMe disk's hidden paths, or a
     disk anything is built on (an old LVM volume group, a RAID array), and **never the installer's
     own medium** — by what is mounted, and by its volume label, so also when the ISO was copied to
     RAM; each with its size, model and how it is attached, and what is on it now;
  3. asks for one, and to type that disk's name again to confirm that everything on it is erased —
     nothing is written before that;
  4. refuses while another disk has a file system labelled as the image's are (`nixos`, `ESP`: an
     earlier NixOS, another ESP), which the installed system, finding its file systems by label,
     could start from or grow instead; opens the chosen disk exclusively and checks it is still the
     disk that was chosen (the kernel's `diskseq`, device number and size: a disk swapped for
     another under the same name is refused, and so is a later opening by name — to read back,
     find the partitions, mount, clear — of another), then writes the image to the whole disk and
     reads it back to check every byte. The image's first MiB, its partition table, is written
     last: the disk's start and end (an earlier table and its backup) are zeroed first and
     everything else written and synced before it, so whatever stops the write — a power cut, a
     reset, the process killed — the disk has no partition table, and no half-written system to
     start from. A failure once writing began — writing, reading back, or detecting the hardware
     below — clears the disk's partition tables (its start, the image's backup, the disk's end).
     No key stops it mid-write: the terminal's signal keys are off (or nothing is offered),
     Ctrl-Alt-Del starts nothing on the installer, and the power, reboot, suspend and hibernate
     keys and the lid do nothing (logind ignores them: it would otherwise stop the installer
     between the write and the hardware file, leaving a system without one) — for the installer's
     whole life, so the power button does not turn it off at the disk menu either: holding it
     down (the firmware's, not logind's) or answering the installer does. If the labels above
     cannot be looked for (blkid failing), nothing is
     written. Disks with sectors larger than 512 bytes are not offered: the image's partition
     table is in 512-byte sectors;
  5. detects this machine's hardware (`nixos-generate-config --show-hardware-config
     --no-filesystems`, on the ISO only — installed machines have no installer tools, 37bed79) and
     writes it, with the image's file systems, into the new disk's root as
     `/var/lib/torq/install/hardware-configuration.nix`;
  6. says to remove the installer and restart.
  On the first boot from the disk the root partition grows to fill it (`boot.growPartition`, as now)
  and first-boot setup runs unchanged, except that it takes the detected hardware file over the
  image's generic one: change #1 records it in the config repo like any machine's.
- **Not now:** installing beside another system or to a partition (whole disks only), disk
  encryption, BIOS boot (UEFI only, as the image), Secure Boot (later, ROADMAP). Both architectures'
  ISOs are built by CI; only x86_64 is installed and booted in a VM test here (ADR-030).
- **Consequences:** One install path to keep correct, not two: what the ISO installs is what the
  image tests already boot. A VM installed from the ISO gets a detected hardware file instead of the
  image's — the same drivers or more. The ISO is larger than the image by its live system and the
  firmware of any machine it may start on (about 1.9 GiB, near GitHub's 2 GiB a release file:
  release.yml refuses one past it, by name). Hardware
  detection is what NixOS's own installer does, so it is as good as NixOS's on any machine;
  firmware a machine needs that NixOS does not detect is a setting to add afterwards
  (`set hardware enableRedistributableFirmware true`). "Nobody can log in" is over the console and
  the network: someone at the machine can edit the boot menu's kernel command line and start a
  shell on the installer — as on any machine whose boot they control, and the installer holds
  nothing (Secure Boot and a locked menu: later, ROADMAP).

## ADR-032 — TorqOS is a skeleton: NixOS's tools gone, the base only what TorqOS runs, on a budget
- **Context:** What TorqOS is: a CLI skeleton you install, boot, and give a configuration that
  builds the actual system — configurations people pass around for every kind of machine. Whatever
  that does not need should not be on the machine. A new machine's own system (the image's) was
  1294 MB in 613 store paths, with 787 commands on its PATH, much of it NixOS's defaults and
  plumbing rather than anything TorqOS runs: `nixos-rebuild` and NixOS's other tools for changing a
  machine, the man pages and their tools, full git with its manuals, Perl and Python for NixOS's
  own scripts, systemd's container-image importer with GnuPG and LDAP, and curl, htop and jq for
  the shell.
- **Decision:**
  1. **No NixOS tools for changing a machine.** `nixos-rebuild`, `nixos-option`, `nixos-version`,
     `nixos-install`, `nixos-enter`, `nixos-generate-config` and `nixos-build-vms` are not on a
     TorqOS machine (the ISO installer keeps `nixos-generate-config`, on the ISO only, ADR-031).
     The broker no longer uses `nixos-rebuild`: it does what `nixos-rebuild switch` did — `nix
     build` the commit, `nix-env --set` the system profile, `switch-to-configuration switch` in a
     transient unit of its own (`torq-switch`, so the switch outlives whatever started it) — as its
     `commit confirmed` path already did. It still counts a switch in `nixos-rebuild`'s unit as
     one it must not race (a system from before this).
  2. **The base is what TorqOS itself runs.** git (the minimal build: the config repo's, read by
     torqcli) and nano (`file edit`'s editor); no documentation, no systemd-importd; htop and jq
     are packages a configuration adds. A configuration turns any of it back on. (curl stays on
     the path: it is one of NixOS's core packages, assembled from several modules, and taking
     one out of that list would mean restating all of NixOS's.)
  3. **A budget.** `checks.x86_64-linux.size` fails when a new machine's system grows past its
     budget (1280 MB): lowered as TorqOS sheds more, never raised to make a change fit without
     saying why here.
- **Kept, on purpose:** Perl (~58 MB), which NixOS's account script needs. NixOS's userborn (Rust)
  would replace it, but does not fit TorqOS's accounts: a person's `hashedPasswordFile` that does
  not exist is an error that fails userborn's whole run (userborn 2026-10 `main.rs`, `password.rs`),
  where NixOS's script locks that one account — and in TorqOS a person without a password file is
  ordinary (SSH keys only; an account just declared; "a file removed is a password removed",
  modules/accounts.nix). A machine would boot with its accounts unmanaged. It also makes
  `/etc/shadow` read-only, which the broker's `chpasswd` writes. Revisit if userborn locks a missing
  file's account instead.
- **Not this step:** Python (134 MB), which only systemd-boot's boot-entry writer needs: a small
  Rust one of TorqOS's own, with its own ADR and tests, since a bootloader bug leaves a machine that
  does not start. The nixpkgs tree (207 MB), which evaluates every configuration, setup's offline
  included: kept, unless setup may need the network.
- **Floor:** a machine that builds itself from a configuration keeps the kernel and its modules,
  systemd, glibc, Nix and the nixpkgs tree. Trimming the kernel would mean every machine compiling
  its own, with nothing from NixOS's cache (ADR-023); overriding big packages (systemd without its
  importer, say) the same: options only.
- **Consequences:** 1294 MB → 1228 MB and 613 → 516 store paths for a new machine (x86_64,
  2026-11-03), Python still to go. Root can still build and switch with `nix`
  and `switch-to-configuration` by hand: removing the tools removes the easy way around the broker,
  not every way (limits on root: later, ROADMAP). Habits from NixOS (`nixos-rebuild switch`) fail
  with "command not found" instead of the `/etc/nixos` guard's message; the guard stays for `nix`
  itself. Tests that read JSON in their VMs bring their own jq.

## ADR-033 — Python leaves with systemd-boot's installer: Lanzaboote now, Secure Boot later
- **Status:** decided, 2026-11-03 (the owner: option 1); **deferred** the same day (the owner),
  because its premise was false — see "Amended" at the end. The VM test it asked for first is
  built and stays (`tests/boot.nix`); Lanzaboote comes with Secure Boot itself.
- **Context:** Python (134 MB, a tenth of a new machine's system) is there for one thing:
  systemd-boot's installer, NixOS's `systemd-boot-builder.py` (452 lines), which writes the boot
  menu at each switch — the kernels and initrds onto the EFI partition, one entry per generation
  and specialisation, `loader.conf` (default, timeout, editor), systemd-boot itself updated with
  `bootctl`, old entries removed past `configurationLimit`. NixOS takes any other installer through
  `boot.loader.external.installHook`, reading each generation's bootspec (`boot.json`).
  docs/SECURITY-MODEL.md already chose Lanzaboote for owner-key Secure Boot (`torq.security.
  secureBoot`, a TODO), and Lanzaboote's installer (`lzbt`, Rust) is itself such a replacement.
- **Decision:** option 1. Lanzaboote (nix-community, Rust) is the boot-entry writer: its `lzbt`
  replaces the Python installer at every switch, bundling each generation into a signed unified
  kernel image under systemd-boot. The keys are the machine's own, made at setup and kept outside
  the config repo; they protect nothing until the owner enrolls them in the firmware, which is the
  Secure Boot work of later (SECURITY-MODEL, ROADMAP) — and then nothing migrates twice. Not option
  2, because bootloader code is the last place to write TorqOS's own and then throw it away; not
  option 3, because the 134 MB buys nothing a machine that will have Secure Boot needs.
- **Options weighed:**
  1. **Lanzaboote now, Secure Boot later (chosen).** Its installer replaces the Python one
     (systemd-boot stays the boot menu, booting signed unified kernel images); setup generates
     the machine's own keys (`autoGenerateKeys`), kept outside the config repo like passwords; the
     owner enrolls them in the firmware when Secure Boot comes (later, ROADMAP) — the same layout
     then, nothing to migrate twice. Costs: an input from outside nixpkgs (nix-community, tracking
     nixos-unstable: pinned to TorqOS's nixpkgs with `follows`), built from source on each machine
     (Rust, minutes) unless TorqOS's releases carry it prebuilt; keys that protect nothing until
     enrolled; a change of boot-entry format on machines already installed (one switch, tested).
  2. **TorqOS's own writer.** A small Rust program (`torq-boot`) doing what the Python one does for
     TorqOS's case (one profile, no XBOOTLDR, no initrd secrets or extra files), same file names
     and layout. Smallest and fully TorqOS's — and replaced by Lanzaboote when Secure Boot comes.
  3. **Keep Python.** 134 MB, no work, no risk.
- **Consequences:** a VM test that boots from a real EFI partition (NixOS's `useBootLoader`,
  `useEFIBoot`) is written **before** the switch and must pass on both writers: the entries after
  a switch, the default, a rollback by choosing an older entry, a `commit confirmed` rolled back by
  a reboot, and the editor off; the image and ISO tests boot through it too. Then the input
  (`lanzaboote`, `follows` TorqOS's nixpkgs; pinned by the weekly bump like nixpkgs), the module in
  `modules/security.nix` (`boot.lanzaboote.enable`, keys generated at setup into
  `/var/lib/torq/secureboot`, fenced from the tree), and the migration of installed machines (one
  switch; `torq5` first). Each machine builds Lanzaboote itself until releases carry it prebuilt
  (ADR-039's cache). The size budget comes down by what Python saved. A second reviewer on the
  diff (docs/DRIVING.md): a bootloader bug leaves a machine that does not start.
- **Amended (2026-11-03): deferred.** Built on a branch (`lanzaboote`, 63cf007, since deleted), it showed the
  premise false: Python is not only the boot writer's — `growpart` (cloud-utils, the root grown
  at the first boot, `boot.growPartition`) needs it too, so Lanzaboote saves none of the 134 MB,
  and adds sbctl and lzbt (1228 → 1277 MB of the 1280 budget). Its fresh reviews found what
  moving every machine would cost now: an installed machine's EFI partition (torq5's: 249 MiB,
  122 MB free) cannot hold Lanzaboote's copies of every generation's kernel and initrd beside
  NixOS's during the switch that moves it; the clean-up of the old writer's entries, as built,
  could remove the only ones that start after a way back to NixOS's writer (a rollback past the
  move); Lanzaboote itself does not test aarch64; each machine downloads a Rust toolchain to
  build its stub. With no saving to set against that, the owner deferred it: Lanzaboote comes
  with Secure Boot (SECURITY-MODEL, later), when keys are enrolled and it is worth its cost, and
  the branch's lessons go with it (a free-space check before the move; a clean-up that removes an
  entry only when its files are gone; x86_64 first). Python leaves when the root is grown by
  systemd-repart instead of growpart — its own item, and then the boot writer is the only thing
  left needing it. Until then the menu is NixOS's writer's, and `tests/boot.nix` (the check
  `boot`) holds it to what this ADR asked of any writer: the entries after a switch, the default,
  an older entry chosen at the menu, a commit confirmed rolled back by a power cut, the editor off.
## ADR-034 — One base, two faces: the sysadmin's server as code, and a person's machine an AI can drive
- **Context:** ADR-019 narrowed TorqOS to a thin, CLI-only OS and deferred the desktop and the AI,
  so the configuration layer could be built first. It is built: the trust loop (M1), torqcli
  (M1.5), the image, setup, signed releases and the ISO (M2). But the reason TorqOS was started
  was never a server. It was an operating system where an AI can be given deep, powerful access
  *safely*: fix a driver, build an application a person asks for, watch a desktop while a program
  is tried — with every change recorded and undoable. What was built is exactly the substrate that
  needs: generations, one writer, the ledger, tiers, a typed tree over every setting, `commit
  confirmed`. What it was being aimed at — an administrator typing torqcli — is one of its two
  users, not the only one. The owner (2026-11-03) wants both: *a sysadmin's dream, servers defined
  in code in git*, and *the laptop or desktop an AI drives*.
- **Decision:** TorqOS has **one base and two faces.**
  1. **The server:** a machine defined in code, in git, driven by `torqcli` and rebuilt from its
     repository anywhere (ADR-036). Everything built so far; nothing taken away.
  2. **The desktop or laptop:** the same base with a desktop *configuration* loaded on it, where
     the person talks to an agent, the agent proposes, and the person approves. The desktop is a
     configuration, not a product: a config repo (`torqos-desktop`: the compositor, a terminal, the
     agent, a few applications) loaded onto the skeleton (ADR-032) like any configuration, with the
     same gate, ledger and rollback. `modules/desktop.nix` becomes that configuration's modules.
  What follows from that:
  - **The AI layers move from "later" to next** after M2 (ROADMAP M5; AI-LAYER.md): doctor mode
    (read, explain, propose nothing), then proposals, then session trust. They need the trust work
    of ADR-035 first, because an agent needs an identity the kernel vouches for.
  - **torqcli is the agent's interface first, the administrator's second.** ADR-025's single
    sources — one typed grammar, `commit check`, every command saying what it did — are what an
    agent drives. Its human polish (the walk of every `?`, usability reviews) stays, lower.
  - **The thin base stays thin** (ADR-032): a small base serves both faces; a desktop adds its own
    size as a configuration, not as the base.
  - **Three scenarios are the acceptance tests** of the laptop face, each a VM test when built:
    (a) *fix my wifi* — a system-tier proposal applied with `commit confirmed`, reverting itself if
    the machine does not come back; (b) *build me an application* — written in the person's home
    (free), installed through the tree at user tier (ADR-037), undone with `rollback`; (c) *watch
    the desktop* — agent tooling over the compositor's screenshot and input protocols, needing the
    person's session and nothing of the broker's.
  ADR-019's **architecture stands; its scope is reversed.** Still out: laptop-*specific* design,
  touch-first, reference hardware, a smoothness target. The desktop is a configuration, not
  hardware work.
- **Consequences:** VISION, ROADMAP, ARCHITECTURE, AI-LAYER, CLAUDE.md rewritten (2026-11-03).
  ADR-035–039 record what the laptop face needs that the server face did not force: closing the
  root gap, the config repo's remote, applications, data, upstreams. The skeleton and ISO work
  continue unchanged — both faces want them. Nothing is deleted; the server face is the base the
  laptop face is loaded on.

## ADR-035 — Closing the root gap: the account, the door, and the record (proposed)
- **Status:** proposed, 2026-11-03; the plan for M3. The order below is the build order.
- **Context:** Root is root: no program can tell a person from an AI typing in a root shell, and
  TorqOS's model already says so (SECURITY-MODEL, "Root, the owner, and an AI the owner runs").
  Today every guarantee is *visibility*, not *enforcement*: the broker trusts `--actor` and
  `--approved-by` as given (KNOWN-GAPS 4, 6a); the ledger is unkeyed and a root writer can rewrite
  the chain (2); `sudo` is needed for daily work, so a root shell is ordinary and silent. The audit's
  N8 and N9 are this: a tty check is not a person, and signing on the machine cannot keep root out.
  The question to answer is not "is this a human?" — it is **"which account is this, and did it
  come through a door that rings?"**
- **Decision:** three levers, each enforceable.
  1. **Identity is the account.** The broker becomes a service on a Unix socket and takes its
     caller's identity from the kernel (`SO_PEERCRED`: uid, pid), never from a flag. The agent runs
     as its own account (`torq-agent`), sandboxed, no sudo, no shell: whatever arrives from that uid
     *is* the agent. Whatever arrives from the owner's uid is the owner — what they run in their
     own terminal is theirs, as a browser extension is. `torqcli` needs no sudo. A system-tier
     commit asks for the owner's **password** at the terminal (on a desktop: a dialog) — the one
     thing a program in the owner's shell does not have; that is the human gate.
  2. **Root rare, and loud.** With torqcli off sudo, the owner can go weeks without root. Then
     **strict mode** (SECURITY-MODEL): sudoers allows TorqOS's own commands and one more, `torq
     break-glass`, which writes a ledger entry *first* (who, when, the reason), anchors it (3), and
     only then gives a shell. Root still exists; it cannot be reached quietly. The agent cannot press
     it: wrong uid, no sudo, no password.
  3. **The record lives where root cannot reach.** After each entry the ledger's head goes off the
     machine — to the config repo's remote (ADR-036), as its own ref; with the remote refusing
     force-pushes, it is append-only in practice. Root can rewrite local history; the anchor
     disagrees, and `verify` says so. Signing entries on the machine is not the defence (N9): a key
     on the box is a key root can read, a TPM key one root can use. What root changes that the
     configuration names does not survive a rebuild (and a store path changed in place, `request system
     repair`, after the daily check); what it leaves elsewhere survives until a reinstall; data it deletes
     comes back from snapshots, if it did not delete those too (ADR-038; the audit of M3, M5).
- **Not:** preventing root. The only OS that does is one with no shell at all (Talos); strict mode
  without break-glass is that, and stays an owner's option for later, once every task an owner
  needs is a torqcli command.
- **Consequences:** Closes KNOWN-GAPS 4 and 6a (identity), 2 (the anchor), restates N8. The
  broker gains a socket service and loses `sudo` as its door; `torq-broker` under sudo stays for
  dev mode and recovery. Measured boot (SECURITY-MODEL) and data snapshots come after, as planned.
  What TorqOS can then say, truthfully: *the agent cannot get root; the owner can, but not
  silently; and what root does here cannot rewrite the record elsewhere, nor, where the configuration
  names it, outlive a rebuild.* (Corrected by the audit of M3, M5: it said "or outlive a rebuild", which
  claimed more than a rebuild looks at.)
- **Amended (2026-11-05): strict mode and break-glass built.** `torq.security.strict.enable`, off by
  default, a choice in the tree. An administrator's sudo runs two things: break-glass and the
  agent's launcher. Not the broker, though this ADR said "TorqOS's own commands": torqcli needs no
  sudo (ADR-040), and the broker's commands under sudo are root's own (it reads any file named to
  it, and sets passwords), so a rule naming it was a quiet way round. Polkit's administrator is root
  alone, and no one may be in a group that is root by another name (docker, disk …). `torq break-glass
  "<why>"` asks the person's password every time, writes the entry, pushes it, and only
  then opens the shell; if the remote cannot be reached it says so and asks. The ledger has its own
  lock, held for one append, so a stuck switch cannot keep the entry out; since an entry can then land
  during an apply, a commit confirmed's timer has an id of its own for each attempt (`confirm.timer`),
  never a ledger number predicted. If the ledger cannot be
  written there is no shell, and no fallback: the owner's decision, "I prefer secure over back doors".

## ADR-036 — The config repo's remote: the machine is a URL
- **Status:** accepted (the owner, 2026-11-05: "yes"); the push and the anchor built 2026-11-05, pull and
  adopt next. Keeps ADR-004's default (local, never pushed) and adds the
  remote as a setting; answers VISION's two "far future" questions.
- **Context:** The config repo is the machine minus its data: `flake.nix`, `flake.lock`, the tree's
  JSON, attached Nix files, `hardware-configuration.nix`, `users/<name>/*.json`. Secrets are kept
  out *by construction* (modules/settings-fence.nix refuses every password form), not by care. So
  the repo can leave the machine without exposing anything but what the machine runs: packages,
  services, user names, public SSH keys, the host name — a private repo, then. The owner's idea:
  point a new install at a repository and the machine rebuilds; and the same remote is the ledger's
  anchor (ADR-035).
- **Decision:**
  - **Adopt at setup.** First-boot setup asks: *a fresh machine, or from a repository?* Given a URL
    and a credential it clones, checks HEAD's signature against the owner's key (enrolled as
    `release-signers` is, ADR-028: trust on first clone, pinned after), takes the repository's tree
    and files but **this machine's hardware file** (the installer's detected one, ADR-031 — hardware
    never travels, and `load override` keeps the local one too), asks for the owner's password
    (it cannot come from the repo; SSH keys do, so a keys-only machine comes back whole), and
    builds: change #1, as any setup.
  - **Push after every applied change**, the ledger as its own ref (`refs/torq/ledger`). Branch
    protection on the remote (no force-push) makes it append-only in practice. `verify` compares
    the local head with the remote's; a difference is an alarm.
  - **Pull is a proposal, never an apply.** A commit on the remote the machine does not have shows
    in `show system remote`; `request system pull` loads it into the candidate; the person reads
    `show | compare` and commits. Nothing from the remote is ever applied unasked — that would be
    the bypass the whole design exists to prevent, and whoever held the credential would own every
    machine.
  - **The credential** lives in the broker's state directory: root-readable, which the model
    allows; unreadable by the agent's account. A deploy key scoped to the one repository,
    push-capable. Root with it can push junk commits (visible, reversible), not rewrite history.
  - **One repository per machine.** Sharing between machines is `save` / `load` (S5), which exists.
    A shared base with per-host overlays — where fleets end up — is a later ADR.
  - The `torq` input must be the releases URL, never `path:` (a dev machine's source means nothing
    elsewhere).
- **Consequences:** *Your machine is a URL*: disk dies, new install, same URL, identical machine
  minus data (ADR-038). Setup gains an online path (the default stays offline). New: `torq remote
  …` settings, `request system pull`, the remote in `show system alarms`. ADR-004's "network is
  download-only" becomes "download-only until the owner names a remote".
- **Amended (2026-11-05): built — the push and the anchor** (`tests/remote.nix`; reviewed). The URL
  is a setting, `torq remote url` (`torq.remote.url`): SSH only — a git URL can run a command
  (`ext::`) — judged by the broker (`url_ok`), git allowed no other transport, no ssh configuration
  read, paths quoted. The credential: a key the broker makes in its state (`request system remote
  key`; a deploy key with write access); the host key pinned at first contact. After every command
  that writes the ledger the broker fetches the remote's ledger and checks it — one line of commits
  (no merge), still holding the commit it checked last, each push since starting with the one
  before, and this ledger starting with the newest — then pushes the ledger, as a commit on top of
  the remote's newest, to the branch `torq-ledger` (a branch, so branch protection keeps it), and
  `main` with it (not while a commit confirmed waits: `main` is never rewound), atomic, never
  forced. Any disagreement — and a ledger gone from the remote, or a changed host key — raises an
  alarm that stays until a person clears it (`request system remote clear`, ledgered) and stops
  the pushes. Every git call to the remote is stopped after a minute. Out of reach, a change still
  lands; the push waits (a timer, 15 min), and a day without one is an alarm.
  **What it is, honestly:** the alarm on the machine is root's to rewrite, as the ledger is. The
  check root cannot reach is the remote's `torq-ledger` branch, protected there (no force-push, no
  deletion, linear history), read from anywhere.
- **Amended (2026-11-05): pull** (`tests/remote.nix`). `request system pull` loads the remote's
  `incoming` branch — never `main`, which only the machine writes: a commit made here from what was
  pulled is not that commit, and `main` is never forced — into the candidate: its settings and
  config files, not this machine's hardware file (hardware does not travel), not its software
  (`flake.nix`, `flake.lock`: `request system update` is how software changes). The broker only
  fetches it (`fetch-incoming`, a read for anyone); the person reads `show | compare` and commits,
  or not. Nothing from the remote is applied by any other path. `incoming` is where a person, another
  tool or a pull request puts what this machine should take; it stays there after. Only people fetch
  it (not the agent, not services: it goes to the network, as root), under a lock of its own, and
  not from a remote whose ledger disagrees. **What travels:** everything but the hardware *file* —
  boot and file-system *settings*, and config files that set them, come with it, shown in the
  compare; read them.
- **Amended (2026-11-05): adopt** (`tests/adopt.nix`; the owner's choice). First-boot setup asks: a new
  machine, or from a repository? The new machine makes its key and shows it; the person adds it to the
  repository — only someone who can administer it can, and that, not a signature, is the trust: the
  broker does not sign its commits yet (signing, and checking it here, is its own item). The
  broker fetches into a staging repository and checks it: the ledger one line, each push extending
  the one before, the chain whole, `main` the commit it ends at; setup shows the machine, owner and
  changes, asks the owner's password (it is not in the repository) and asks before anything
  changes. An adopted machine **continues** the old one: its repo history and ledger, then one
  commit putting in this machine's hardware file and the image's software (`flake.nix`, `flake.lock`:
  a repository's may name a source that was the old machine's own), and one entry, `adopted`. So the
  remote's ledger stays this one's start, and pushing goes on. From its review: adopted is exactly
  what was shown — the commits and owner are pinned, nothing is fetched after the question — and a
  repository is refused for a link at any of this machine's own files (written without following
  anything), a path no one may change, no hardware file read, a commit confirmed still waiting in
  its ledger (which would drive the boot's rollback, as root), or a size past reason. The console
  shows the commits and the host key's fingerprint to compare, and says what is being given: this
  configuration runs here as root, SSH on or off, who has keys, which Nix files it attaches.

## ADR-037 — Applications: a ladder, every rung declared (proposed)
- **Status:** proposed, 2026-11-03; the design for M4's applications. Rung 3 needs its own small
  design before the agent builds anything.
- **Context:** A desktop needs Firefox and LibreOffice, a vendor's proprietary application, and what
  the person or the agent builds. An installer that puts a binary somewhere and on PATH does not
  work on NixOS (no `/lib`: the binary cannot find its libraries — KNOWN-GAPS, "Dev mode", `claude
  update`) and goes unrecorded even when it does. "Applications" are three things: the application
  (the tree's), its settings and data (the home's, never the tree's), and an application built here.
- **Decision:** a ladder; the repository is the whole truth on every rung.
  1. **nixpkgs, in the tree** — the default. `users <name> packages`, `environment systemPackages`;
     pinned by `flake.lock`, byte-reproducible, rolled back with the system. Most of a desktop.
  2. **Flatpak, declared in the tree** — the escape hatch, for what nixpkgs lacks, lags on, or a
     vendor supports only there: `users <name> flatpaks [ com.spotify.Client ]`, reconciled at each
     switch (a module like nix-flatpak's, exposed like any option), never `flatpak install` by hand.
     The repository records the application and its remote, not its bytes — acceptable because a
     Flatpak is a leaf nothing depends on, sandboxed, with its own rollback.
  3. **Built here** — a package from a repository of the person's own: the source in their home
     (free, the agent iterates there), a small `flake.nix` the agent writes, and the tree pointing
     at it (`users <name> projects [ ./projects/foo ]`, or a git URL). User tier, so build, install,
     test and undo need no approval and each is a ledger line. Today the tree takes nixpkgs names
     only: this rung is the design to do.
  **Never:** `curl | sh`, self-updaters, AppImage, Snap — refused where TorqOS can see them
  (`set` of a path outside the repository already is), documented where it cannot.
  Unfree software stays as it is: allowed by name (modules/unfree.nix), on the person's machine by
  the person's choice; never in an image (ADR-039).
- **Why not Flatpak for everything** (Silverblue's answer): it loses reproducibility for the thing
  most worth reproducing, the agent cannot easily build one (a Nix package is twenty lines in the
  language it already uses), and nothing is shared between applications on disk.
- **Consequences:** the desktop configuration turns on `services.flatpak` and the portal for its
  compositor; disk for runtimes; the agent builds Nix packages, not Flatpaks. The repository shows
  every application on the machine, by rung.

## ADR-038 — Data: what a rebuild does not bring back, and what TorqOS does about it
- **Status:** proposed, 2026-11-03; local snapshots accepted and built 2026-11-06 (the owner: btrfs in
  new images, snapshots on system changes only), the backup and sync parts still proposed.
- **Context:** The repository rebuilds the machine, not the person's life on it. A laptop's data is
  five kinds: documents and media (catastrophic to lose); application settings (annoying);
  secrets — SSH and GPG keys, saved passwords, tokens (serious, and never in the repository);
  caches (nothing); and, on servers, service data in `/var/lib` (catastrophic). ADR-004's
  "optional user-chosen encrypted backup" is this decision.
- **Decision:**
  - **Documents are a sync service's**, declared in the tree so a rebuilt machine comes back
    syncing — `services syncthing` (NixOS's own, peer-to-peer, no cloud) or a mount of OneDrive or
    the like (rclone). TorqOS does not build one: it is the honest stance of every OS that treats
    the machine as disposable (ChromeOS, macOS with iCloud, Silverblue).
  - **A declared backup.** NixOS's `services.restic.backups` as the tree's backup of `/home` and
    `/var/lib` to any target (S3, a NAS, SFTP, a disk), encrypted; its key lives where passwords do,
    outside the repository. `request system restore` on a rebuilt machine; setup's from-a-repository
    path (ADR-036) offers it. A rebuild is then two steps, both ledgered: the repository, then the
    restore.
  - **Local snapshots.** On a snapshot-capable file system (btrfs, the installer's choice at
    partition time), the broker snapshots `/home` and `/var/lib` before each commit, and `rollback`
    offers the data too: the agent's migration of a database undone with the service. This is also
    the root gap's "what a root process deletes" (ADR-035, ROADMAP).
  - **Secrets never enter the repository** (the fence); a password manager is recommended for
    application credentials; public keys are in the repository already, private ones in the
    encrypted backup or regenerated and re-enrolled. There is no design without one secret in a
    human's head — the backup passphrase — and TorqOS makes it exactly one.
  - **Caches:** nothing.
- **Consequences:** A laptop's whole state is three pointers — a repository URL, a backup location,
  a sync account — and the machine comes back from them: system bit-for-bit, applications,
  settings, data. To build: `request system restore`, the restore step in setup, the snapshot hook
  in the broker, the installer's file-system choice. Everything else is NixOS options the tree
  already has, plus telling people the honest thing.
- **Amended (2026-11-06): local snapshots built.** Every new machine is on btrfs: the image and the
  ISO, one layout, no choice at install. `/var/lib/torq` is a subvolume of its own, so no snapshot of
  `/var/lib` holds the ledger, the configuration or the password hashes: restoring data can never
  rewind the record. Snapshots are read only and root's alone, taken before each system change only (a
  person's own settings would let anyone push the others out), the newest ten kept, the label in the
  change's entry. A snapshot that fails stops nothing and is said in the entry: a disk too full to
  snapshot may need a change to clear it, and a snapshot is a way back, not a guarantee.
  `rollback` does not bring data back by itself, as this ADR first had it: putting a live service's
  files back under it is a person's decision. `show system snapshots`, then `request system snapshot
  restore <n> <path>`, an administrator's, with their password, ledgered. Root reads the snapshot into a
  pipe and the owner of the folder writes it: the owner holds nothing of the snapshot, which a
  descriptor would let them walk up, and root writes nothing into a folder of theirs:
  it is theirs to fill with links, and a root copy into
  it would go where they point. It goes to a name beside the target, private until done, and what is
  live changes by a rename once it is all there: a failure leaves it as it was. What was there is set
  aside, not overwritten. The broker's lock covers the checks and not the copy, so an owner who stops
  their half stops no one else. Set-id
  bits never come back, and the group is the account's now. Machines already on ext4
  (torq5 among them) have no snapshots, and nothing moves them.
  `torq snapshots keep` and `enable` are a person's, in the tree: a system change, never the agent's; what the
  broker runs for them stays fenced. `request system snapshot take` and `delete <n>` are an administrator's,
  with their password. A delete removes a way back, so its entry comes first, as a password's does.

## ADR-039 — Upstreams: what TorqOS owes, and what it depends on
- **Context:** TorqOS redistributes images made of other people's software: nixpkgs and NixOS's
  modules (MIT), the kernel and git (GPLv2), bash and coreutils (GPLv3), systemd and glibc (LGPL),
  redistributable firmware. It fetches from the NixOS Foundation's binary cache and GitHub
  repository; a desktop's applications come from Flathub, an independent nonprofit. None of these
  licences restrict what the software is used for — field-of-use limits are not open source — and
  "a different use case" owes nothing. *Distributing binaries* does. This is a maintainer's map,
  not legal advice.
- **Decision:**
  - **A source statement on every release** (release.yml): the tag, the nixpkgs commit, that
    `nix build` of them builds the same system from the same sources (not the same bytes: an image
    carries its own copy of its source), and that every package's source is pinned there by hash.
    The sources themselves are upstream's, fetched by those hashes: a mirror of them is for when
    TorqOS has users (below), and until then the GPL's "corresponding source" rests on them.
  - **Nothing unfree in an image or the ISO**, checked by CI: `torq.unfree` is off in `hosts/image`
    and `hosts/installer`, and a check fails if anything would let a package nixpkgs marks unfree
    into them (what it covers and cannot see: Consequences).
    Dev mode's Claude Code never ships. On a person's machine, what *their* configuration fetches
    from a vendor is their download, not TorqOS's distribution: the image is TorqOS's to answer
    for; the configuration is the owner's. Firmware stays `enableRedistributableFirmware`'s set —
    the word is the legal term.
  - **Trademarks:** "a NixOS derivative" (ADR-029), never NixOS or its logo; nixpkgs' Firefox
    unmodified (Mozilla's permission covers it), rebranded if ever changed.
  - **Owner keys for Secure Boot** (SECURITY-MODEL) stay the plan: GPLv3's anti-tivoisation
    permits exactly that and forbids project-held boot keys.
  - **The dependencies are a bet, named.** The code cannot be withdrawn (MIT, pinned, and the
    nixpkgs tree is inside every image). The *services* can change: `cache.nixos.org` (ADR-023's
    bet) and `github.com/NixOS/nixpkgs` have no SLA, and the cache's cost is a recurring Foundation
    concern; Flathub serves every desktop distribution and gates no client. If the cache went away
    nothing breaks at once — Nix builds from source, slowly. When TorqOS has users: mirror nixpkgs
    to `netstacks`, a caching proxy in front of the cache (which is also where TorqOS's own pieces
    get prebuilt, the footnote ADR-023 allowed for), a source mirror. And send fixes upstream:
    projects do not cut off the people who help.
- **Consequences:** One paragraph in release.yml and one CI check now; the mirrors when there is
  anyone to serve. ADR-023 gains the footnote. KNOWN-GAPS lists the bet. Built 2026-11-03: the
  release notes' **Source** paragraph (the tag, the nixpkgs commit, the command that builds the
  image from them), and `checks.<system>.free` — nothing lets an unfree package into the image or
  the ISO, on both platforms: none of nixpkgs' settings that would (`torq.unfree`, `allowUnfree`,
  `allowUnfreePackages`, `allowlistedLicenses`, `handleEvalIssue`), in them or a specialisation
  of them; no package set from outside; no second nixpkgs reached in TorqOS's own Nix
  (`pkgs.path`, `<nixpkgs>`, NixOS's modules' parent, however spelled), and no licence set but
  its own packages' (a package relabelled free would pass nixpkgs' check). nixpkgs
  refuses an unfree package otherwise, so that is the closure's licence check, as far as nixpkgs'
  licences go: a package with no licence check (a vendor's file fetched as it is) it cannot see,
  and redistributable firmware nixpkgs counts as free, which the ISO carries (linux-firmware and
  others, `enableRedistributableFirmware`) and the image does not. It checks itself: each of the
  eight ways planted in the image is caught. (Review of 2026-11-03: the first version checked only
  `torq.unfree`.)

## ADR-040 — The broker on a socket: who is asking is the kernel's answer
- **Status:** accepted, 2026-11-04 (the owner: both the password at system changes, dev mode included, and PAM in the broker); ADR-035's first lever, the first item of M3. Built on a branch,
  reviewed fresh (docs/DRIVING.md) before `main`.
- **Context:** Today torqcli reaches the broker through `sudo torq-broker …` and tells it who is
  asking: `--actor`, `--approved-by $(id -un)`, `--tainted` (KNOWN-GAPS 4). Anything running as a
  wheel user can therefore commit as that user, and only wheel users can use torqcli at all; user
  tier is not bound to the user it names (6a); the broker opens the `--diff` and `--hash-file`
  paths it is given, as root (N16). The human gate for a system change is sudo's password — gone
  in dev mode, and the same password that opens every other root door.
- **Decision:**
  1. **A socket, activated by systemd.** `torq-broker.socket` listens on `/run/torq/broker.sock`
     (mode 0666: who may do what is decided inside, by uid, not by who can connect);
     `torq-broker.service` runs as root with today's hardening, started on demand, one connection
     at a time (the broker's lock already serialises changes). The protocol is one request per
     connection, a length-limited JSON line, and the reply a stream of output lines and an exit
     code — a build's progress shows as it happens, and a client that hangs up does not stop a
     switch already started. A change's diff, a password hash, a reason travel **in the request**,
     never as a path: the broker opens no file a caller names (closes N16).
  2. **Identity from the kernel.** The broker reads `SO_PEERCRED` (uid, pid) and decides the actor
     from the uid alone: the `torq-agent` account is the agent; any other non-root account is a
     user, by name; there are no `--actor` or `--approved-by` on the socket. `--tainted` stays, but
     only raises: a caller may mark its own proposal untrusted, never clear a mark the broker set.
     torqcli runs without sudo and without `torq-broker` on its PATH.
  3. **Who may do what** (the tier, `broker/src/policy.rs`, is the change's; the caller is the uid):
     - *observe* (`log`, `verify`, `status`, `show`): any local account.
     - *user tier*: a user's own `users/<name>/` only — a change to another's is system tier (6a).
     - *system tier*: the owner (`torq.user`) and any account the configuration makes an
       administrator; applying one asks that account's **password, checked by the broker through
       PAM** (`security.pam.services.torq-approve`: pam_unix with faillock, so guessing is slowed and
       logged). torqcli reads it from the terminal with echo off and sends it in the request; the
       broker checks it, forgets it, and ledgers who approved. A script in the owner's shell has the
       uid but not the password; that is the human gate ADR-035 asks for. No terminal, no system
       change — as today.
     - *the agent*: propose only; never apply, confirm, revert or reconcile; never a password
       check (its uid has none). What it may propose is the next item (the allow-list, KNOWN-GAPS 6m).
     - *forbidden*: refused for everyone, as now.
  4. **sudo stays the recovery door, and says so.** `sudo torq-broker …` keeps working for dev mode
     and recovery (a broken socket, a broken PAM), its entries ledgered as `via sudo` with the
     invoking user sudo reports (`SUDO_UID`) — what root says about itself, marked as such. Strict
     mode (ADR-035) was built narrower than this: `torq break-glass` and the agent's launcher, not the
     broker (ADR-035, amended).
  5. **Built from maintained parts.** systemd socket activation (no listener code of TorqOS's
     own); PAM through a maintained Rust binding (its `unsafe` is the binding's, not TorqOS's
     crates', as rustix's is — ADR-020); `SO_PEERCRED` through rustix. torqcli marks itself
     non-dumpable while it holds a password, so another program of the same uid cannot read it out.
- **Tested** (a VM test, `tests/socket.nix`, each would fail if its claim broke): a user commits
  their own folder without sudo; the same user cannot change another user's folder, or the
  system, without their password; a wrong password is refused and faillock counts it; `--actor`,
  `--approved-by` or a file path in a request is refused; the agent account can propose and can do
  nothing else; observe works for every account; a client killed mid-switch leaves the switch to
  finish and the ledger consistent; `sudo torq-broker` still works and is ledgered `via sudo`.
- **Not:** a daemon of TorqOS's own that listens on the network, ever; polkit (its agent and
  rules engine are more than this needs on a server, and the desktop's dialog can talk to the same
  socket, M4); signing requests (the uid is the kernel's word; a key would be root's to read, N9).
- **Consequences:** Closes KNOWN-GAPS 4 (identity), 6a (user tier bound to its user), N16; the
  system-tier gate becomes the owner's password even in dev mode (dev mode keeps passwordless
  sudo, for building TorqOS, not for approving changes). The CLI's `sudo` handling
  (`cli/src/main.rs` `BROKER`, the "sudo asks for your password" errors) goes. A2 comes next on the
  same branch or the one after: with requests no longer root's own, the broker re-fetches each
  changed input itself and checks its hash and signature.

## ADR-041 — The agent's account: `torq-agent`, and what it may propose
- **Status:** accepted (the owner, 2026-11-04: "do it"); 1–4 built 2026-11-05, 5 designed and next. ADR-035's account lever, after ADR-040 (the socket). The owner:
  people will drive TorqOS with the AI tools they already use — Claude Code first — so the account
  is made to be one such a tool can run as, not only TorqOS's own agent (M5).
- **Context:** ADR-040 made identity the kernel's: whatever arrives from the `torq-agent` uid is
  the agent, and it may only propose. But nothing makes that account yet (the socket test creates
  one by hand), and a proposal from it may still touch anything a person's could: the settings
  fence (`modules/settings-fence.nix`) keeps out the broker's own inputs, not the settings that
  run code as root — a service's script, sudo rules, tmpfiles, binary caches and their keys
  (KNOWN-GAPS 6m). A person reviews each proposal, but a review is not a boundary; a default-deny
  list at the broker is.
- **Decision:**
  1. **The account** (`modules/agent.nix`, always present): a static system user `torq-agent` —
     static, because the broker names its caller from /etc/passwd (a DynamicUser is not there,
     KNOWN-GAPS 6zf) — in its own group, no password, no login shell (`nologin`: no SSH, no
     console), in no other group (no `wheel`, so no sudo; no device groups). Its home,
     `/var/lib/torq-agent` (0700, its own), is where a tool keeps its state and credentials (Claude
     Code's `~/.claude`), so they are never the owner's. It reaches the system only through the
     broker's socket, as torqcli does; what it runs, it runs with its own uid.
  2. **What it may propose: a default-deny list at the broker**, checked on every proposal from the
     agent's uid, by what the change *does*, not only which files it touches:
     - **Never:** anything outside the tree's settings — the config files (`config-files.json` and
       the Nix files it attaches), `flake.nix`, `flake.lock`, the hardware file — and anything in
       `torq.*` (the agent's own limits live there) or the fence's list.
     - **A person's packages** (`users/<name>/packages.json`): the owner's (`torq.user`) by default;
       `torq.agent.mayPropose.users` names others.
     - **Settings** (`nixos/<top>.json`, `system/<name>.json`): only the subtrees
       `torq.agent.mayPropose.settings` lists (default: none — the agent may propose packages and
       nothing else until the owner says otherwise). The broker compares each JSON file before and
       after the change and checks every setting that changes against the list: `services.openssh`
       allows `services.openssh.settings.PasswordAuthentication`, not `services.nginx`.
     - The list is the broker's (`/etc/torq/broker.json`, from the module), and `torq.*` is never on
       it, so the agent cannot propose widening its own list.
  3. **A refusal is ledgered** (`kind: refused`: who, which files, why, the diff's hash — not the
     diff), so an agent that tries is on the record, not only turned away.
  4. **People see and approve the agent's proposals in torqcli:** the agent's `commit` proposes and
     stops (`proposed as #n: a person applies it`); `show system proposals` lists what waits;
     `request system apply <n>` shows its diff (and whether its session read untrusted content),
     asks, and applies it — with the person's password for a system change, as their own commit.
  5. **A tool like Claude Code, run as the agent (built 2026-11-05, `tests/agent-run.nix`).**
     `torq agent run [--share <dir>] [--] <command>` — `torq agent run --share ~/project -- claude`.
     An administrator starts it without a password; sudo runs `torq-agent-run` as root, which, in a
     mount namespace of its own, binds the shared directory where it is — checked to be the very
     directory torqcli opened as the person (device and inode of the descriptor it is handed), and
     theirs — hides every home around it, and then drops to `torq-agent` for good (`setpriv`: its
     uid, no capabilities, no new privileges) before running anything, in bubblewrap: the people's
     runtime directories hidden, /tmp its own, the agent's home its home, network on. Sharing is an
     ACL on what of the directory is the person's (default ACLs, so files either makes stay both
     of theirs); the agent gets no permission on the home itself (the owner's choice, 2026-11-05: a
     pass-through ACL let the agent read the home's world-readable files by name). A sandbox of the
     tool's own (Claude Code's bubblewrap) runs inside; nothing it starts outlives it (its own process
     namespace); the share is nosuid and nodev; one session at a time while sharing. **What a share
     means:** whatever the agent writes there may later run as the person — a git hook, `.envrc`, a
     Makefile, a package's scripts, a tool's own hooks (`.claude/settings.json`) — so a share is
     trusting the agent with the person's next `git commit` there. Not settled: what "tainted" means for a tool
     that reads the web (the agent still marks its own).
- **Tested** (`tests/agent.nix`, each failing if its claim broke): the account exists as stated (no
  password, nologin, no groups, its home 0700); as it, a proposal of the owner's packages is
  accepted; a system setting not on the list, a setting on the list beside one that is not, a config
  file, `flake.nix`, `torq.*`, and another person's packages are each refused **and ledgered**; with
  `mayPropose.settings = [ "services.openssh" ]` an `openssh` change is accepted and an `nginx` one
  refused; the agent's torqcli `commit` proposes and stops; a person's `request system apply`
  applies it, asking the password for a system change.
- **Not:** the agent applying anything, ever (ADR-040); the launcher (5) in this step; the agent's
  model, tools or prompts (M5).
