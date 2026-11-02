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
    (`TORQOS_RELEASES_TOKEN`, a fine-grained token: it expires, at most a year).
  - **Not covered yet:** an attacker able to move `stable` back to an older signed release
    (a downgrade): `stable` only moves forward when CI moves it, and `show | compare` shows the
    version before a commit, but the machine does not refuse an older release by itself. The check
    is torqcli's, at update: a person who edits the lock by hand is past it (as they are past any
    check, being able to commit).
