# Known gaps

Open issues found while bringing up M1 (2026-10-30). Fix with a failing test first.

## Broker / trust model
1. ~~**Crash between commit and ledger append.**~~ Fixed (ADR-010): reconciled on the next
   operation. Still manual: leftover git locks other than `index.lock` (`refs/heads/main.lock`,
   `HEAD.lock`, `packed-refs.lock`) after power loss, and HEAD not descending from the recorded
   commit — both refuse loudly; inspect `/var/lib/torq/config`, fix, rerun `reconcile`. A
   process that inherits the broker lock and never exits blocks the broker until it is killed
   (the error names it). Not yet: reconcile at boot; recording who triggered a reconcile beyond
   `reconciled_by`; edits made in the repo *during* a rebuild are still reset by a failed
   apply's restore (only reconcile snapshots them). The broker's git runs no hook and no fsmonitor of
   the repo (`core.hooksPath=/dev/null`, `core.fsmonitor=false`; second audit, M1: fixed).
2. **`verify` cannot detect truncation of the newest entries, and the chain is unkeyed** (second
   audit, H2): it is sha256 over each entry and the one before, so a writer of the file (root)
   can rewrite the history and recompute the chain; it does not cross-check git `Ledger-Entry`
   trailers against the ledger, and commits are not signed (L1). What it does catch: an edited,
   added or removed field or entry (an added field passed before the audit of 2026-10-31).
   Planned (M3): sign the entries and commits with a broker key, anchor the head hash.
3. **Path-based tiers cannot see semantic escalation inside system-tier files** — e.g.
   `torq.broker.enable = false`, a new `torq.broker.stateDir` (fresh empty ledger),
   `torq.security.lockdown = "none"`, or swapping `torq.url` in `flake.nix`. Today only
   human review of the diff catches these. Needs a design (evaluate the old and new config and
   diff protected options?).
4. **Caller-supplied identity:** `--actor`, `--tainted` and `--approved-by` are trusted as given.
   (Configuration no longer is: a root broker takes its repo, state directory, rebuild command and
   PATH/HOME only from `/etc/torq/broker.json`, refuses a flag or `TORQ_*` variable that differs,
   and clears its caller's environment — ADR-016, piece 1e.) Fine under sudo in M1; the M3 socket
   must take identity from `SO_PEERCRED`. `torqcli` inherits this: it passes `--actor user
   --approved-by $(id -un)` itself, so anything running as the user (a script, an AI tool in their
   terminal) can commit a change to their own folder (`torqcli -c`) or revert one; it needs `sudo`
   for the broker, so only wheel users can use it until the socket exists. A system-tier commit
   (or revert) needs a terminal,
   which stops background jobs but not anything started from the user's terminal. The per-user
   candidate is user-writable, so its integrity between `show | compare` and `commit` rests on the
   user's account; the agent must never write a user's candidate (it proposes as `actor=agent`).
   The second audit (H1, H5) is this item: the approver and the actor are whoever the caller says,
   so ledger attribution, and the agent's restrictions (which key on `--actor agent`), hold only
   for callers that do not lie. Not fixed by patches: the M3 socket takes identity from the kernel.
   The broker under sudo is not sandboxed either (L3; the socket service will be).
5. **`torq.url = "path:…"`** in the config template ignores `.gitignore`, so `broker/target/`
   is copied into the store on every evaluation. Prefer `git+file://`. (Fixed for the config repo
   itself: the broker builds the exact commit, `git+file://<repo>?rev=<commit>` (`{flake}`), never
   its working tree, so "running system = ledgered commit" holds by construction; second audit,
   H4.) A `path:` source (the dev VM's)
   is allowed only with `torq dev enable true` (second audit, M3).
   Since each system keeps its sources (`lib.mkSystem`, so a configuration can always be evaluated
   again), the dev VM's generations each keep a copy of the checkout with its `target/` folders
   (~770 MB). The fix is the one above: the dev source as `git+file:///home/<you>/torq` (committed
   files only, `.gitignore` respected).

6a. **User tier is not bound to a user:** `users/alice/packages.json` is user tier whoever proposes
   it, so an agent session acting for viper could change alice's apps. Needs the caller identity
   from #4 (M3 socket). A user's packages also act in that user's *session*: autostart entries,
   D-Bus session services and systemd user units from their profile (never system-wide).

6b. **Drift detection has blind spots** (ADR-012): a lock that never frees (a leaked holder) makes
   every check "busy" and silent; drift is reported but not written into the ledger, so an outside
   switch followed by a broker apply leaves no trace in the audit trail (`repair` does ledger the
   drift it fixes); the warning reaches interactive shells (and `show system alarms`) only. `repair` checks after its switch that the running system and next boot agree and
   that no other switch is running, but an outside switch that both starts and finishes between
   its own switch and that check would set both alike and pass; comparing against the exact path it
   built would close that.
6e. **OPEN (high): an outside commit made while a change is interrupted is discarded untraced.**
   If HEAD has moved past the interrupted operation's own commit (someone committed on top of it),
   `show system alarms` shows only "a change was interrupted" (the CLI hides head findings while
   interrupted) and `request system repair` → `reconcile_locked` keeps and ledgers only `pending.commit`, then
   resets: the outside commit survives only in the reflog. Fix: broker `keep(head)` and ledger it
   (`discarded_head`) when `head != pending.commit`, with a test; expose `interrupted.commit` in
   `status --json`; the CLI hides the head finding only when HEAD is `before` or that commit.
   Also low: `history::asked_by` credits a failed/interrupted *undo* to the original change's
   actor; undo is user-only, so rollbacks should show "user". (Found by review of f2da8b7.)
6f. **The template's flake.nix is not evaluated.** tests/tree.nix evaluates the template as a whole
   machine (its data, its hardware file attached, an owner set, as setup does: S4a), but through
   `torq.lib.mkSystem`, not the template's own flake.nix, so a flake.nix that stopped exposing
   `nixosConfigurations.torq` would only show at the next install. A config
   repo made from an older template (output `torq-utm`) must be renamed to `torq` before its broker
   is updated; only the dev VM ever had one. Such a repo also sets `networking.hostName` in
   `machine.nix`: adding `system/host.json` with a host name to it is a conflict until that line
   goes (the template now keeps the host name only in `system/host.json`).
6g. **`propose --base` is optional for a person.** Without it a diff applies wherever it applies,
   so a stale change can still mix with a newer commit. `torqcli` always passes it; the broker
   requires it for `--actor agent` (second audit, M2), and nothing builds on a commit that is not
   the last ledgered one (audit of 2026-10-31). The base must be a 40-hex SHA-1 id: a config repo created with git's SHA-256 object
   format could not use it.
6j. **torqcli and a malformed tree file.** torqcli refuses to load a tree file it cannot show
   faithfully (wrong shape, unknown key, a value with spaces), so one bad file blocks `configure`
   and `commit` for everyone until it is fixed — by a raw diff through `torq-broker propose`.
   Evaluation normally keeps such files out of HEAD (a change that does not evaluate is undone), so
   it takes a hand-made system-tier change. (Tested in the VM: an ordinary user reads the root-owned
   repo, and a hostile `GIT_*` environment is ignored.) Not yet tested end to end: a `torqcli`
   commit through the real broker — the test VM's config repo does not evaluate; the cutover's
   config repo will.
6k. **torqcli's line editor drops keys typed ahead.** rustyline reads input in chunks and starts each
   prompt with a fresh buffer: keys typed (not pasted) before the next prompt appears are lost. A
   bracketed paste is kept whole and run line by line. Two interactive sessions of the same user
   share one saved candidate, the last save wins.
6i. **User tier: whose folder?** The broker accepts `users/<name>/<leaf>` for any well-formed name;
   that `<name>` is a normal user of the machine is checked only at evaluation (modules/packages.nix),
   so `users/root/packages.json` is refused late — by the failed rebuild, which is undone — not at
   propose. And a user-tier change may write *any* user's folder: `approved_by` is not tied to
   `<name>`. Today that only lets one user change another's package list; before a
   leaf that matters more joins the user tier, tie user-tier approval to the folder's owner (with
   the socket's `SO_PEERCRED`, #4) and check the name against the machine's users at propose.
6h. **`commit confirmed` limits.** The timer's broker still takes its repo and state directory
   from the *new* system's `/etc/torq/broker.json` (read leniently); its restore commands and PATH
   are the anchor's own. A change that moves `torq.broker.configRepo` or `stateDir` would leave the
   timed rollback looking in the wrong place (taking them from the anchor's file would mean trusting
   a settings path from the caller); a reboot still rolls it back (the boot default is never
   changed before `confirm`). `--confirm` builds the config again even if `commit check` just built
   it (cached by nix, but evaluated twice). "Boot default" is the system profile, not the boot
   loader's entry: a profile set while the boot-loader install failed counts as restored. The
   activation is not run under `systemd-run` as the broker's plain switch is (`torq-switch`): a change that kills the
   user's session kills the broker mid-switch (recovered by reconcile, but noisy).
6c. **`request system update` trusts the user's fetch:** `nix flake update` runs as the user, so their
   environment (proxy, CA, `NIX_CONFIG`) shapes the narHash written to the lock, and root builds
   that store path without re-fetching. The ledgered rev may then not identify what was built.
   Fix: the broker re-fetches changed locked inputs as root and compares narHashes.

6d. **Service limits** (ADR-013; services are NixOS's own since ADR-025): the SSH key check and the
   key listing run in the CLI only (at commit and revert); an agent proposing
   `services.openssh.enable` skips them (the broker cannot see home directories) — keep
   `~/.ssh` out of any future user-tier dotfiles, or "plant a key, then enable ssh" becomes
   possible. Tailscale keeps its own settings (Tailscale SSH, exit node) in `/var/lib/tailscale`,
   outside the ledger, and re-enabling it rejoins with the old identity. Firmware updates (fwupd)
   are not offered until they can go through the broker (SECURITY-MODEL.md). The terminal-safe
   printing is duplicated in the broker and the CLI (`printable` / `history::clean`). The SSH key
   listing shows authorized_keys files and declared keys only, not other ways sshd can admit
   someone (`AuthorizedKeysCommand`, `TrustedUserCAKeys`, `AuthorizedPrincipalsFile`), which TorqOS
   does not pin either (modules/services.nix sets only defaults, root's login excepted).
   The old catalog (`services.json`, `torq ssh passwords`) still works with a warning; it goes
   once no machine has it.

6n. **A mistake in how the machine starts shows only at the next restart** (review of S4a). The
   tree sets `fileSystems`, `boot` (loader, initrd modules) and `swapDevices` like any setting, and
   the attached hardware-configuration.nix is plain Nix: a wrong but well-formed value (`fileSystems
   /boot fsType ext4`) builds, switches, and fails at the next boot, into an emergency shell.
   `commit confirmed` cannot help (its timer runs on the new system, which never comes up). torqcli
   says so in the commit question; the way back is an earlier configuration in the boot menu (20
   kept), which needs the machine's console, and `boot loader timeout 0` hides the menu. To do: make
   the new boot entry one-shot until a confirmed commit is confirmed; refuse a zero timeout.

6o. **A flake.lock that does not match flake.nix is fetched before it is refused** (review of S4b).
   The broker builds with `--no-update-lock-file`, so such a commit never builds, but to find out
   that the lock needs changing Nix first fetches the new source, as root (its `inputs` are read,
   nothing is built). torqcli never commits a mismatch (a lock belongs to the flake.nix it is
   committed with); a hand-made proposal still could.

6p. **"TorqOS's fixed flake.nix" is recognised by its one `torq.url` line** (audit of 2026-10-31).
   torqcli does not check the rest of it (outputs, `configDir`, no extra modules or inputs): a
   flake.nix changed by hand beyond that line is still shown as TorqOS's own. It is changed only
   through the broker, as a system-tier change a person reads.
6q. **`| display set` loads back, with three exceptions:** a value that is exactly `|`, or a list
   value that is exactly `[` or `]`, is refused when read back (a quoted word is not told apart from
   the syntax). `save` / `load override` keep them. `load set` saves the candidate as it goes:
   all or nothing for errors, not for a crash or a full disk midway.
6r. **The config repo is readable by every user** (0755/0644; second audit, L4), and so is the Nix
   store it is built into: a secret must never be in it. M2's first-boot setup must keep passwords
   (even hashes, which can be cracked offline) out of the repo, and its images need checksums
   or signatures of their own (a signed binary cache covers store paths, not a qcow2 or ISO).
   Proposed answer: SECURITY-MODEL.md, "Accounts and passwords".
6s. **An owner can hand root to any program** (`sudo bash`, then an AI tool in it): no software can
   stop that. What TorqOS does about it — visible, recoverable, unnecessary — and what is still
   open (the broker telling a person from a program, #4; a ledger root can rewrite, #2; snapshots
   of data; strict mode): SECURITY-MODEL.md, "Root, the owner, and an AI the owner runs".
6t. **A power cut mid-build can damage Nix's caches** (`/root/.cache/nix`, the broker's HOME): Nix
   writes them without waiting for the disk, and then every later build fails with "database disk
   image is malformed". Found by cutting the power during first-boot setup, which now clears them
   when it starts over (nothing else uses them before change #1). On a set-up machine it stays
   manual: remove `/root/.cache/nix` (only a cache; rebuilt as needed). The broker could do that
   itself when a build fails that way, with a test that damages the cache.
   (The store itself is safe from this since ADR-024: `fsync-store-paths`.)
6u. **Packages cannot be found by the command they install** (`rg` → ripgrep, `nvim` → neovim).
   `show packages <words>` searches names and descriptions (`nix search` on this machine's
   nixpkgs); which package holds a command needs an index that is not on the machine (nixpkgs'
   `programs.sqlite` comes with channels, not flakes; nix-index downloads one). Offline it is not
   possible today.
6v. **Package names are checked only as far as the schema has them.** The schema holds nixpkgs'
   top-level names (`builtins.attrNames pkgs`, nothing evaluated): a name in a set is checked by
   the set only (`python3Packages.nosuch` passes `set` and fails at `commit check`), and a top-level
   name that is not a package (a function such as `fetchurl`) passes too. The names are the running
   system's nixpkgs, not the candidate's after `request system update`.
6w. **Entries of a list without names renumber on delete.** `delete networking interfaces eth0 ipv4
   addresses 1` makes 2 the new 1: a pasted script that deletes one and then sets another by its old
   number changes the wrong entry. `show | compare` shows what each number now holds before commit.

6x. **What a setting's default is, when nothing sets it, is not shown.** `show` says "Nothing is set
   at …", and `set services openssh ports 2222` does not say it replaces the default 22. Reading
   every setting's default when the schema is built fails: some defaults raise errors Nix cannot
   catch (`builtins.tryEval` catches only `throw` and `assert`; a missing attribute, or an option
   "accessed but has no value defined", ends the evaluation), NixOS's own documented defaults
   included. Reading one when it is asked for would mean evaluating the configuration (seconds).
   Possible: defaults written by NixOS's manual build, or an evaluation per setting in its own
   process.
6y. **Reading the history asks for the owner's password out of dev mode** — decided: the history is
   privileged (ADR-026).
6z. **A group that does not exist is quietly ignored** (found 2026-11-02, on torq5) — fixed: warned.
   - **What happens:** `set users users viper extraGroups docker`, with Docker off, commits without
     a word and changes nothing. NixOS fills each group's members from the people who name it
     (`users.groups.<g>.members`), so a group nothing declares has no member list, and the name is
     dropped. Installing `docker` as a package does not make the group either: only
     `virtualisation docker enable true` does.
   - **What there is now:** `show users` shows the groups each person is really in, and says where
     groups are set and that `docker` needs Docker on.
   - **Fixed (2026-11-02):** a warning (decided: not a refusal, which would also stop a commit that
     turns Docker off while someone is still in `docker`). modules/accounts.nix makes each such group
     a NixOS warning saying what brings it (Docker's) or how to declare it; `commit check` and
     `commit` now show the configuration's warnings (all of NixOS's, which were dropped before),
     `commit` with the comparison before it asks. Tested: tests/tree.nix (`groups-missing`,
     `groups-docker`, the template), the session's tests.
6za. **Password guessing over SSH was never blocked** (found 2026-11-02, by tests/ssh.nix) — fixed.
   - **What happened:** with passwords allowed (setup's "password" or "either"), sshguard ran and saw
     nothing: NixOS has it read the journal's `sshd` lines, and OpenSSH (9.8 on) logs logins as
     `sshd-session`. Only OpenSSH's own few seconds' penalty slowed a guesser.
   - **Fixed:** modules/services.nix has sshguard read both. tests/ssh.nix checks every SSH
     protection: keys only by default, the owner only, never root, and a guesser's address in the
     firewall's block set, a right key refused after.
6zb. **Setup's first question could be lost from the screen** (found 2026-11-02, by tests/image.nix on
   x86_64) — fixed.
   - **What happened:** a graphics driver loading after setup started (bochs-drm on QEMU's
     standard VGA, as on Proxmox; a GPU's on real hardware) took the console over, and what was on
     it was gone: a screen asking nothing while setup waited. Ordering setup after
     systemd-udev-settle (20e1b3d, in v0.1.1) did nothing: NixOS has no such unit. Waiting for
     udev's queue does not help either: the driver loaded after it was empty.
   - **Fixed:** while it waits for an answer, setup shows its question again (with the welcome)
     when the console changes size, as a takeover does. tests/image.nix makes one on purpose.
     A takeover that keeps the size would still lose the screen; Enter then answers with the
     default shown in brackets, and the next question appears.

## Audit findings to fix (2026-11-01)

Two audits of 2026-11-01, kept as their authors wrote them: `docs/AUDIT-2026-11-01.md` (H, M, L)
and `docs/FABLE_AUDIT.md` (N, plus its check of the first). Each is fixed when the work it belongs to
comes up, not all at once; this list is where they are tracked. Already elsewhere here: M1 is 6e, M7
is 6p, N18 is 6 (below).

| # | What | Status |
|---|---|---|
| H1 | First-boot setup not crash-resumable | fixed (dfae2d3): setup.json, finished at the next start |
| H2 | SSH lockout check only when SSH turns on | fixed (f44bd94): `session::ssh_review` on every change |
| M5 | `path:` source guard reads JSON, not the evaluated configuration | fixed: an assertion in modules/dev.nix (the configuration's own lock) |
| M6 | Setup accepts a key that only looks like one | fixed: `ssh-keygen -l` decides (`pkg::key_id`) |
| N1 | **high:** a `_`-named `nixos/*.json` file (`_type`, `contents`) gets past the fence | fixed: a file there is named after a top-level NixOS option, never `_` (modules/nixos-settings.nix; tree: nixos-fence-underscore-file) |
| N2 | **high:** `users extraUsers` (NixOS's alias) gets past the password rule; root can get a password | fixed: the aliases are fenced (settings-fence.nix `aliases`); an assertion keeps root without a password, a config file included (tree: nixos-fence-alias-users, config-root-password) |
| M2 / N12 | A missing boot profile is not a problem to `status`; `repair` counts it as fine | open (apply/restore half fixed) |
| M3 | A password change can take effect without its ledger entry; no fsync of the folder | open — one transaction |
| M4 / N13 | Outside-switch checks are a moment's; `confirm` has none (nor the unledgered-HEAD check) | open |
| L1 | Proposal file names printed unescaped | fixed: escaped as everything the broker shows |
| L2 | `XDG_STATE_HOME` taken without an owner check | open |
| L3 | A save file may hold a file and its deletion marker | open |
| L4 | Nested `top` without limit | fixed: a command is never inside itself (`top top` refused; the grammar's `Walked::next`, for `?` too) |
| N3 | A torn ledger append (power cut, full disk) stops every broker command, recovery too | open — write-ahead copy, repaired and ledgered |
| N4 | `propose --reason` may hold newlines: forged git trailers | fixed: a reason is one line, no control characters (refused at propose; torqcli sends one clean line) |
| N5 | Only torq-broker-init is sandboxed; SECURITY-MODEL says all are | open — sandbox the root units, fix the sentence |
| N6 | `systemd tmpfiles` and `environment.etc."nixos/*"` reach the broker's inputs and the /etc/nixos guard | open — fence them |
| N7 | Setup offers password SSH by default; no test of the image's invariants | fixed: none by default (Enter turns nothing on); tests/image.nix checks the image, tests/ssh.nix SSH's protections |
| N8 | "A person at a terminal" is a tty check: `script`/`expect` pass it | open — say so (KNOWN-GAPS 4); absolute `id` |
| N9 | Signing the ledger on the machine cannot keep root out | open — restate: signed by the build, anchored off the machine |
| N10 | `save <file>` writes anywhere (`.ssh/authorized_keys`) | open — confine save/load |
| N11 | The broker takes `$6$` alone as a hash | fixed: crypt's whole form (`policy::password_hash_ok`) |
| N14 | `TORQ_NIXOS_SCHEMA` honoured in release builds | not so: only a test reads it; torqcli reads /etc/torq/nixos-schema.json, fixed |
| N15 | Helpers found through PATH (`nix`, `git`, `id`, `ssh-keygen`, …) | open — absolute paths |
| N16 | The broker reads `--diff`/`--hash-file` without an owner check or O_NOFOLLOW | open (matters with the M3 socket) |
| N17 | The /etc/nixos guard stops the default path only; CLAUDE.md says more | CLAUDE.md reworded; the guard is as SECURITY-MODEL says |
| N19 | Every proposal's diff is in the ledger; each call parses it all | open — diffs by hash beside it |

The audit of 2026-11-04 (end of M2, `docs/AUDIT-2026-11-04.md`), kept as written:

| # | What | Status |
|---|---|---|
| A1 | The release check accepted an OpenPGP- or X.509-signed commit (git handed it to gpg and the caller's keyring) | fixed: only an SSH signature, checked before git verifies; gpg's and X.509's programs `false`, no caller's home (cli/src/release.rs, a test with a PGP-signed commit) |
| A2 | A lock's content hash, not its signed `rev`, decides what is built: a lock someone else wrote can pair the newest signed rev with older source in the store | open — the broker re-fetching each changed input as root and checking its hash and signature (M3, with the socket broker); SECURITY-MODEL says what is not covered |
| A3 | `stable` could move to an older release; `show \| compare` shows dates, not versions | partly: CI refuses a release not newer than `stable`'s version (release.yml). Open: torqcli showing and comparing versions |
| A4 | First-boot setup ledgered change #1 when the switch failed before the new system took, leaving the image with no account | fixed: ledgered only if the new system runs or is the boot default; otherwise nothing set up, setup asks again (broker/src/lib.rs `set_up`, its test) |
| A5 | Release authority wider than the key: the releases token in the bump job's environment, any `v*` tag published | open — a protected environment for release, the token in the step that needs it (GitHub settings) |
| A6 | The release check says nothing for a source other than the releases repo; `github:` accepted for it | open |
| A7 | `systemd.suppressedSystemUnits`, a `systemd.mask=` parameter and `torq.setup.enable` switch off the drift check | open — fence them, or assert the units |
| A8 | The installer's disk is bootable without its hardware file for as long as the write takes | open (6zc's window, wider than it says) |
| A9 | An interrupted setup leaves a password file under the first name typed | open |
| A10 | Ctrl-C at a setup question ends setup until the next boot | open — `Restart=always`, or the console's signal keys off as the installer has them |
| A11 | Release hygiene: a tag moved mid-run signed untested; loose pre-release pattern; no revocation; the fingerprint in one place | partly: the tested commit is what is signed (a moved tag fails), tags are `vX.Y.Z` or `vX.Y.Z-word` only. Open: revocation, the fingerprint elsewhere |
| A12 | Claims stronger than the code (verify, signed updates, signed modules, sandboxing, rebuilds, compare) | fixed: README, SECURITY-MODEL and ADR-028 say what is true |

Its design notes (§4) are for the AI layer and M3: `SO_PEERCRED` names an account, not a person;
approve-once for user tier; taint set by the tool layer; what strict mode can mean; where an update
credential lives; the broker checking the `path:` rule itself; the threat table's "later" items.
Its §5 lists claims without a test.

## Security defaults that are not real yet
6. **`lockdown=integrity` does nothing:** the kernel command line has it, but the lockdown LSM is
   not active (`/sys/kernel/security/lsm` = `capability,landlock,yama,bpf,ima`). Module signing
   is not built into the kernel either, so "signed modules only" (SECURITY-MODEL.md) is not true.

## Upstreams (ADR-039)
6u. **`cache.nixos.org` and `github.com/NixOS/nixpkgs` are a bet, not a right.** Every machine
   fetches from the Foundation's cache (ADR-023) and `flake.lock` points at GitHub; neither has an
   SLA, and the cache's cost is a recurring Foundation concern. The *code* cannot be withdrawn (MIT,
   pinned, and the nixpkgs tree is inside every image); the services can change for everyone. If
   the cache went away, Nix builds from source — slowly. Answer when there are users: a nixpkgs
   mirror under `netstacks`, a caching proxy, a source mirror. Not yet: the release's source
   statement and CI's unfree check (ROADMAP M2).

## Desktop (off in the base; the torqos-desktop configuration's, ADR-034, M4)
7. niri runs its stock config: `Mod+T` spawns `alacritty` (not installed; we ship `foot`), no
   `xwayland-satellite` (X11 apps fail), no cursor theme. Ship a TorqOS niri config.
8. ~~Template hard-codes user `casey`.~~ Fixed: `torq user` (nixos/torq.json) is the one place
   (asserted set and well-formed); per-user apps are `users/<name>/packages.json`.

## Dev host quirks (not TorqOS bugs)
- **ESXi must expose nested virtualisation** or there is no `/dev/kvm`, and every NixOS VM test
  falls back to QEMU's TCG emulation (the trust loop goes from seconds to minutes). VM → Edit →
  CPU → "Expose hardware assisted virtualization to the guest OS", with the VM powered off.
  Check with `ls /dev/kvm` and `grep -o vmx /proc/cpuinfo`.
- **No aarch64 machine of its own** since the Mac VM was retired (ADR-030): an arm64-only failure
  cannot be reproduced by hand, only through CI's arm64 runners. If one appears and resists
  iterating through pushes, borrow an arm64 machine rather than guessing.

### UTM on a Mac (historical: that VM was the dev host until 2026-11-03, ADR-030)
- niri needs 3D: UTM display card must be `virtio-gpu-gl-pci` with GPU acceleration on
  (renderer ANGLE/Metal). Without it: black screen, "software EGL renderers are skipped".
- Firefox's GPU WebRender drops most text on virgl. Workaround: `LIBGL_ALWAYS_SOFTWARE=1 firefox`.
- On a Mac keyboard, niri's Mod is ⌘: ⌘⇧Q logs out of macOS and ⌘Q quits UTM. Use UTM's
  "Capture input" before using niri shortcuts.

## Dev mode (`torq.dev.enable`)
- **Claude Code's own installer and `claude update` do not work on NixOS.** The native binary is
  a plain dynamically-linked ELF (dies with the `stub-ld` error) and the installer edits
  `~/.bashrc` to put it first on `PATH`, which breaks `claude` in every new terminal. Instead,
  `modules/dev.nix` pins the version by overriding nixpkgs' release manifest with
  `modules/claude-code-manifest.json`. To update, fetch a newer manifest (the commands are in
  `dev.nix`) and ship it through the broker. Running sessions keep their old code until relaunched.
- **A `path:` TorqOS source builds the working tree, not a commit.** `nix` copies the directory
  as it is on disk for a `path:` flake input; it does not read git. So on a dev machine with
  `system software torq source path:/home/viper/torq`, uncommitted edits in the checkout are built
  and ledgered like any other change: recorded, attributable and reversible, but not reproducible
  from the repository — the ledger names a store path nobody else can rebuild. `git status` before
  shipping. (Found on torq5, 2026-11-03, while writing ADR-030's setup.)
6l. **`show system alarms` has no exit code of its own.** `torq status` exited 1 when the machine was
   not as its ledger says and 75 while a change was being made; `torqcli show system alarms` says it
   in words and exits 0. Scripts and monitoring use `sudo torq-broker --json status` (its `ok`,
   `busy`, exit codes), which is what `torq status` read. Pending-change detail (`torq log <n>`) has
   no torqcli view yet beyond `show system ledger`. (Found by review of 4g.)
6m. **The fence is not the AI's boundary.** modules/settings-fence.nix keeps the broker's own inputs
   out of the configuration tree's data and refuses module directives (`_type`) in it, but settings
   that run code as root — a service's script, sudo rules, wrappers, tmpfiles, activation scripts,
   binary caches and their keys, extra groups — stay settable: ordinary system-tier power when a
   person approves the change, and a list of forbidden paths can never be complete. What an agent
   may propose has to be a default-deny allow-list at the broker, by top-level and subtree, keyed on
   the agent's identity (#4, M3). (Found by review of S3a, which also showed the bypasses now
   tested in tests/tree.nix.)
   Config files (`file edit`, `set <place> config <file>`, S3e) are plain Nix and are not fenced at
   all: like system.nix always was, they can set anything, the broker's settings included. A person
   approves each one as its raw diff; an agent proposing one is for the same M3 allow-list (a
   config file is a whole module, so the allow-list will have to refuse them to agents outright).
6zc. **The ISO installer refuses a machine with another NixOS disk.** The image finds its file
   systems by label (`nixos`, `ESP`), the same on every TorqOS disk and NixOS's defaults, so a
   second disk carrying either could be what the installed machine starts from or grows. `torq
   install` refuses until that disk is disconnected or cleared (ADR-031). Unique labels or
   partition UUIDs per install would lift this. Also: hardware NixOS detects as needing an unfree
   driver (a Broadcom wifi card's `broadcom_sta`) gets it named in the detected hardware file,
   which TorqOS, allowing no unfree packages, then fails to evaluate at first-boot setup; untested
   on such hardware. (Found by review of the installer, 2026-11-03.)
   Smaller, from the second review (2026-11-03), not fixed: a stick blkid cannot probe (copied to
   RAM, a read error) is offered, since only its label says it is the installer's; the hardware is
   detected after the write, so a detection failure costs the disk's contents (detecting first
   would wipe nothing); disk model strings are printed as the disk reports them (an escape
   sequence in a USB disk's INQUIRY string could redraw the list); a cciss disk's `cciss!c0d0`
   name is not mapped to its `/dev/cciss/c0d0`. And the installer does not ignore SIGINT, SIGQUIT
   or SIGTERM itself (Rust has no safe way without a new crate): with the console's signal keys
   off and logind ignoring the power, reboot, suspend and hibernate keys and the lid (since
   2026-11-03: before, the power key was a poweroff that stopped the installer between the write
   and the hardware file, leaving a system without one), only root sends one — nobody logs in to
   the installer — and a write stopped by one leaves no partition table (ADR-031 step 4).
   One window remains (review of 5f99d05): on a disk exactly the image's size, the image's own
   backup partition table lands at the disk's last sector near the end of the stream, so a power
   cut in the seconds before the first MiB is written leaves a disk a firmware could restore the
   table from — holding everything the image has, but not yet read back. Holding back the image's
   last MiB too, written with its first, would close it.

6zd. **The skeleton's loose ends** (review of `slim`, ADR-032, 2026-11-03; none a defect) —
   closed the same day: a broker test fakes an active `torq-switch.service` as one fakes
   nixos-rebuild's unit (`apply_and_revert_wait_for_the_brokers_own_switch_unit`); the switch's
   `-E LOCALE_ARCHIVE` is gone (the broker's environment never had one to pass); the boot menu's
   editor lock is checked on the disk the installer writes (`editor 0` in its `loader.conf`,
   tests/installer.nix) until the EFI-partition test (ADR-033); the image test checks htop and
   man are not there (curl is one of NixOS's core packages, and stays: ADR-032); the wording naming `nixos-rebuild` as what the broker runs is gone from
   `user-docs/`, modules/broker.nix and broker/src/lib.rs; modules/base.nix says man pages need
   `documentation man enable true` as well; and trust-loop starts a real `torq-switch` unit by
   hand and checks the broker applies nothing until it stops. Still open: no test would notice
   `-E LOCALE_ARCHIVE` put back (harmless: it passes nothing).

6ze. **The fence's loose ends** (reviews of 2026-11-03). torqcli's own check (cli/src/nixos.rs
   `fenced`) knows the fence's patterns but not where an /etc file lands, a `..`, or that
   `specialisation` is refused: such a setting is refused at the commit, when the system is
   evaluated, with the module's message, not when it is typed. An /etc entry whose `source` is a
   glob is linked under its files' own names, not its target, and is not checked for where those
   land (no way to make one land in /etc/torq from the tree was found). The unit fence covers
   `torq-*` services, timers and units, not sockets or paths (the broker has none yet).
