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
2. **With a remote, a cut or rewritten ledger is an alarm (ADR-036, 2026-11-05, `tests/remote.nix`):**
   the remote holds every ledger pushed, and this one must extend the newest (`status`). Without a
   remote — the default — what follows still holds. **`verify` cannot detect truncation of the newest entries, and the chain is unkeyed** (second
   audit, H2): it is sha256 over each entry and the one before, so a writer of the file (root)
   can rewrite the history and recompute the chain; `status` checks each change against its commit (who approved it, which
   proposal; by the entry's `commit`, as the `Ledger-Entry` trailer can be one short, an entry from
   break-glass or a restore landing during an apply), so one rewritten alone is seen. Both rewritten
   together are not, and commits are not signed (L1). A person who has looked into a disagreement says
   so (`request system ledger acknowledge`), on the record. What it
   does catch: an edited,
   added or removed field or entry (an added field passed before the audit of 2026-10-31).
   Planned (M3): sign the entries and commits with a broker key, anchor the head hash.
3. **Path-based tiers cannot see semantic escalation inside system-tier files** — e.g.
   `torq.broker.enable = false`, a new `torq.broker.stateDir` (fresh empty ledger),
   `torq.security.lockdown = "none"`, or swapping `torq.url` in `flake.nix`. Today only
   human review of the diff catches these. Needs a design (evaluate the old and new config and
   diff protected options?).
4. **Caller-supplied identity — closed on the socket (ADR-040, 2026-11-04, `tests/socket.nix`).**
   The broker's socket takes the caller from `SO_PEERCRED` and refuses `--actor`, `--approved-by`
   and any file path; torqcli uses it, without sudo; a system change asks the caller's password
   (PAM). What stays: `sudo torq-broker` (dev mode and recovery) is root, and still trusts the
   flags it is given — its entries are marked `via: sudo`; `--tainted` can still only be set by the
   caller (it only raises). The history of this item, as it stood before:
   `--actor`, `--tainted` and `--approved-by` are trusted as given.
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
   The broker under sudo is not sandboxed either (L3), and neither is its socket service yet (ADR-040: it runs as under sudo, a development source in a home included) — see 6zf.
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

6a. **User tier is not bound to a user — closed on the socket (ADR-040):** a person applies their
   own folder (`users/<them>/`) without a password; anyone else's needs an administrator and their
   password. Before: `users/alice/packages.json` was user tier whoever proposed
   it, so an agent session acting for viper could change alice's apps. A user's packages also act in that user's *session*: autostart entries,
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
6y. **Reading the history** was the owner's alone, with a password, out of dev mode; since the socket
   broker (ADR-040) it is anyone's on the machine to read, and only the broker writes it. Formerly: the history is
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
| M3 | A password change can take effect without its ledger entry; no fsync of the folder | fixed: the entry first, `password_failed` after it if the change fails, saying whether the running system has it; the folder synced. Stopped between the two, the ledger says too much, never too little. A crash after the running system has it and before it is kept leaves no `password_failed`, and the next start or build brings the old one back |
| M4 / N13 | Outside-switch checks are a moment's; `confirm` has none (nor the unledgered-HEAD check) | open |
| L1 | Proposal file names printed unescaped | fixed: escaped as everything the broker shows |
| L2 | `XDG_STATE_HOME` taken without an owner check | open |
| L3 | A save file may hold a file and its deletion marker | open |
| L4 | Nested `top` without limit | fixed: a command is never inside itself (`top top` refused; the grammar's `Walked::next`, for `?` too) |
| N3 | A torn ledger append (power cut, full disk) stops every broker command, recovery too | fixed: a last line with no end is read past and said by `status`, and goes at the next entry or push, atomically, its bytes in the entry that says so (`ledger_mended`). One the hash vouches for gets its newline back. A whole line that does not verify is an edit: nothing touches the ledger then |
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
| N16 | The broker reads `--diff`/`--hash-file` without an owner check or O_NOFOLLOW | closed on the socket (ADR-040): a diff or hash comes in the request, written by the broker to its own 0700 directory; `sudo torq-broker` is root reading root's choice |
| N17 | The /etc/nixos guard stops the default path only; CLAUDE.md says more | CLAUDE.md reworded; the guard is as SECURITY-MODEL says |
| N19 | Every proposal's diff is in the ledger; each call parses it all | open — diffs by hash beside it |

The audit of 2026-11-04 (end of M2, `docs/AUDIT-2026-11-04.md`), kept as written:

| # | What | Status |
|---|---|---|
| A1 | The release check accepted an OpenPGP- or X.509-signed commit (git handed it to gpg and the caller's keyring) | fixed: only an SSH signature, checked before git verifies; gpg's and X.509's programs `false`, no caller's home (cli/src/release.rs, a test with a PGP-signed commit) |
| A2 | A lock's content hash, not its signed `rev`, decides what is built: a lock someone else wrote can pair the newest signed rev with older source in the store | open — the broker re-fetching each changed input as root and checking its hash and signature (M3, with the socket broker); SECURITY-MODEL says what is not covered |
| A3 | `stable` could move to an older release; `show \| compare` shows dates, not versions | mostly fixed: CI refuses a release not newer than `stable`'s version (release.yml); torqcli names TorqOS by version in `show \| compare` and at `commit` (`torq 0.2.1 (2026-11-05) -> 0.2.2 (2026-11-07)`: the running one from /etc/os-release, the release's from the commit fetched to check its signature, no second fetch: its number from `cli/Cargo.toml`, its tag, a pre-release included, from the signed message) and says an older number is a downgrade, whatever the dates (a pre-release and its release: by date, as the running side never says a pre-release) (cli/src/release.rs, session.rs `software_is_shown_as_versions`); a source other than the releases, or one without a version, still by date. Open: refusing an older version or a pre-release on `stable` unless the person confirms |
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
7. ~~niri runs its stock config.~~ Moot: the desktop is COSMIC (ADR-045); `modules/desktop.nix`
   still carries niri until M4 rewrites it.
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
6m. **The fence is not the AI's boundary — the agent's list is (ADR-041, 2026-11-05, `tests/agent.nix`):** the broker
   refuses every proposal of the agent's account that changes a setting outside `torq.agent.mayPropose`
   (none by default), or anything that is no setting, and ledgers the refusal. What stays as below
   is the fence for people's own changes. Before: modules/settings-fence.nix keeps the broker's own inputs
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

6zf. **The socket broker's open ends** (ADR-040; its adversarial review, 2026-11-04). Fixed from
   that review: faillock's counts in a root-only directory (`/run/torq/faillock`), so the person
   cannot reset their own; password checks one at a time per account (not yet exercised by a test); `apply --confirm` an
   administrator's (it holds everyone else's changes back); services' accounts propose nothing;
   the whole request within 10 s; the reply through a bounded queue, so a caller that stops reading,
   or reads too slowly, is dropped and the broker never waits on it (the second review); four connections per
   account; every entry records the kernel-given `caller`; Nix's build users (uid 30001 on) are not
   people; and sudo, su and passwd lock as the broker does (the owner's decision, 2026-11-04:
   `torq.security.passwordLockout`, the same count in the same root-only directory). Open:
   - **The agent as a DynamicUser** (ai.nix's plan) would not be in /etc/passwd, which the broker
     reads to name its caller: the agent's account (M3's next item) is a static system user.
   - **Proposals are not limited.** A person (or the agent) can propose as often, and as large
     (8 MiB a request), as they like, and every proposal stays in the hash-chained ledger for good:
     a ledger, and disk, that only grow. Wants a limit of open proposals per account and a rate.
   - **`status` holds the broker lock** while it checks the whole ledger: anyone looping it can
     make others' changes wait and fail as busy (3 s). Check before taking the lock.
   - **An own-folder `apply` first runs `reconcile`**, which can finish an interrupted setup or
     roll back an interrupted commit confirmed — ledgered as approved by that person, though
     `reconcile` itself is an administrator's. It only restores the ledgered state.
   - **`via` is what the broker was told.** A wheel member may `sudo TORQ_VIA=socket …`, and `su -`
     sets no SUDO_UID: `via` and `caller` are evidence only for entries a non-root caller made over
     the socket; root can write anything.
   - **Reading is everyone's:** every account — the agent's and services' included — sees every
     proposal (unapplied too), each person's package list, the reasons. Nothing secret is in the
     ledger (password hashes are kept outside it), but it is wider than root-only was.
   - **Not sandboxed**: `torq-broker@` runs as the broker under sudo does (a development source is
     built from a home). N5's sandboxing comes with the root units' own.
   - A caller that reads too slowly (the reply queue) is not yet exercised by a test; nor is
     a switch that changes `torq-broker.socket` while an instance runs.

6zg. **The agent's account, first step (ADR-041).** Fixed from its adversarial review (2026-11-05): a
   person is shown what a proposal *does* — the diff git writes of its result, recorded by the broker
   when it is proposed — never the proposer's text, which git can apply differently (lines past a
   hunk's count); every refusal of the agent's is ledgered, the forbidden ones and the diffs that do
   not apply included, at most 30 an hour; its reasons at most 200 characters, at most 20 of its
   proposals waiting; its list checked again at apply; paths given to git literally; a bare `system`
   refused. From the second review: no refusal is ledgered before the ledger has begun (it would
   have stopped setup); only proposals that can still land count toward the twenty; the shown
   diff is forced to text, and git's own files (`.gitattributes`, `.gitignore`, …) are refused for
   everyone. Open ends:
   - **`torq agent run` (ADR-041, 5) has a root first step**: `torq-agent-run`, a shell script, runs
     as root to bind the shared directory, then drops to the agent. It is small and checked (the
     descriptor's device and inode, the owner, setpriv), but it is root code a person's arguments
     reach — the kind the broker's rule says should be Rust. And the sandbox is bubblewrap's with
     the network on: a tool run as the agent can reach anything the machine can.
   - **A share runs as the person later.** What the agent writes in a shared project — a git hook,
     `.envrc`, a Makefile, a package's scripts, `.claude/settings.json` hooks — runs as the person
     the next time they use it there, and the person is an administrator. Not blocked (it is what a
     shared project is); a review of what changed there (`git status`, `.git/hooks`) after a session
     is the defence, for now. The agent can also make its own files there unreadable to the person
     (it owns them); the person can still remove them.
   - **The agent as a service** (M5) runs as the same account: `torq agent run --share` refuses while
     any agent process runs, so a running agent service will block sharing until that is designed.
   - **The agent's own `commit`** (it proposes and stops) is tested in torqcli's unit tests, not in a
     VM: the VM tests' configurations are stubs torqcli cannot evaluate.
   - **Stale proposals stay listed.** A proposal prepared against an older configuration is refused
     at `request system apply` (the configuration has moved on), but `show system proposals` still
     lists it; there is no way to withdraw or expire one.
   - **The list is by setting, not by what a setting does.** `services.openssh` allows every option
     under it — `services.openssh.extraConfig` included. Listing a subtree is trusting the agent
     with all of it; the narrower the list, the safer.
   - **`tainted` is the proposer's word.** The agent marks its own proposals as having read untrusted
     content; a proposal without the mark proves nothing. It should come from the session that runs
     the agent (M5), not from the agent.
   - **Past 30 refusals an hour, the rest leave no entry** (a cap, so that refusals cannot fill the
     ledger): an agent can spend the 30 and then probe unrecorded for the hour. One summary entry
     ("N more not recorded") would close it.
   - **A refusal's hash is of the request's file as the broker re-reads it**: over the socket that is
     the broker's own copy (root's 0700 directory); under sudo, root's word.

6zh. **The remote's open ends (ADR-036, the push and the anchor; its review, 2026-11-05).** Fixed
   from it: a merge in the remote's ledger history is refused (a rewrite set beside it passed a
   first-parent walk); the history is checked from the last commit verified, one ledger at a time;
   every git call to the remote stopped after a minute (a remote that answered but never finished
   held the broker's lock); `main` not pushed while a commit confirmed waits (its rollback would
   have forked it); the ledger commit made on the remote's newest; the alarm sticky until a person
   clears it, ledgered, and raised too for a ledger gone from the remote or a changed host key;
   a day without a push an alarm; the wording blames neither side; paths quoted. From the second
   review: `main` never pushed while an operation is unfinished, and pushed apart from the ledger
   (a `main` the remote refuses — another history there — is a problem of its own, and no longer
   keeps the ledger from it); a ledger the remote holds but that cannot be read is a disagreement;
   each URL has its own record (a new one starts afresh); a day after pushes began to fail, an
   alarm. Open:
   - **What the anchor needs from the remote:** on `torq-ledger`, for the deploy key, no
     non-fast-forward update and no deletion. Without them root can replace the remote's history.
     torq5's remote (`netstacks/torq5-config`) is private and **unprotected** for now: GitHub keeps
     branch protection on private repositories for paid plans (the owner's choice, 2026-11-05).
     Protection, when added: `deletion`, `non_fast_forward` on every branch.
   - **A check from elsewhere must walk every step.** Root may still append any fast-forward commit;
     a verifier off the machine must check each push extends the one before, not only the tip.
   - **A remote, or a man in the middle on first contact, can latch the alarm** (a "host key
     changed", a history it rewrote): pushes stop until an administrator clears it — a denial of
     service on the owner, never a way in.
   - **The machine's alarm is root's.** Root can delete `alarm.json` or `verified`, as it can rewrite
     the ledger. The record root cannot reach is the remote's protected `torq-ledger` branch; a check
     of it from elsewhere (another machine, a CI job) is what makes the anchor root-proof.
   - **First contact is trusted.** The host key is pinned when first seen (`accept-new`); a host key
     given with the URL would close it. A changed key is an alarm; the fix, when it is a rotation,
     is to remove `remote/known_hosts` as root and clear the alarm.
   - **Not yet tested in a VM:** a commit confirmed rolled back with a remote (unit reasoning only).
   - **The key's creation is not ledgered** (`remote-key`); only the URL, a setting, is.
   - **Pull (`request system pull`)** takes the remote's `incoming` branch, settings and config files
     only. `incoming` is not cleared after: a second pull of it loads nothing new (the candidate is
     the configuration in effect). `show system remote` does not yet say whether `incoming` has
     something this machine has not taken.
   - **A pull's fetch has no size cap** (only a time limit): a remote, or whoever writes its
     `incoming`, can put large objects into the config repo; they stay until git collects them.
   - **The pulled configuration is evaluated before the commit question** — as on any commit, by
     the person's torqcli (`nix eval`, pure): its Nix may build what it imports (import from
     derivation) before anyone says yes. It cannot reach root.

6zi. **Adopt at setup (ADR-036).** Open:
   - **First contact is trusted, and SSH proves nothing about the deploy key.** Anyone answering as the
     host on first contact is pinned, and can accept any key and serve a forged repository: the
     console shows the host key's fingerprint and the commits to compare, and only that person's
     comparing stops it. Pinned host keys for common hosts, or the old ledger's hash typed in, would.
     Each retry trusts first contact again (a changed key shows once as an error, then is re-pinned).
   - **The summary reports the tree's SSH settings, not everything.** Other settings and attached
     files can turn SSH on or do anything else: the console says so. Only an evaluation of the whole
     configuration, shown before the question, would be complete.
   - **The staging fetch has a time limit, no size limit**, and the checks after it none of time.
   - **The trust is the deploy key.** Whoever can add a key to the repository decides what a new
     machine becomes; commits are not signed yet, so nothing on the machine can tell a repository
     rewritten by its host from the owner's. The ledger's checks show it is consistent, not whose.
   - **Only the owner's password is asked.** Other people in the configuration have none on the new
     machine until `request system password`.
   - **A broker that dies mid-adopt** after the switch and before the ledger goes in leaves the new
     system running with no ledger (setup's own record of a cut-short setup does not cover adopt).
   - **`torq.dev.enable` and every other setting travel** with the configuration: adopting a dev
     machine makes a dev machine. The summary does not call it out.

6zj. **Strict mode and break-glass (ADR-035).** Open:
   - **No ledger, no shell, by the owner's decision (2026-11-05: "I prefer secure over back doors").**
     If the entry cannot be written (a ledger edited or damaged; a power cut's torn line is mended, N3;
     a full disk is not one any more, since the record has a file system of its own, the audit of M3, M2)
     break-glass refuses, and with strict mode on sudo offers nothing else.
     Mending a torn line needs room for a copy of the ledger, on the record's own file system.
     This is a limit accepted, not a gap to close: do not add a fallback shell. The way back is the
     console: an older generation from before strict mode, if one is left (they are limited and
     collected, and an adopted machine may have none), and otherwise external media.
   - **An older generation is also a way round.** A person at the console who boots a generation from
     before strict mode has its sudo, and root with no entry. `status` reports the drift afterwards;
     nothing is ledgered. Physical access is outside what strict mode claims; measured boot is its answer.
   - **"Yes" at the question leaves the entry on this machine alone.** A person who cuts the network,
     answers yes and edits the ledger as root leaves the remote no trace of that shell. The remote's
     check sees a rewritten ledger only if it has already seen the entry.
   - **What the shell does is not recorded,** only that it opened, who opened it and why.
   - **It keeps root rare, not absent.** A person with the password can still commit a configuration
     that runs anything as root: that is a change, ledgered, and it is meant to work.

6zk. **Snapshots of the data (ADR-038).** Open:
   - **Root can delete them, and so can an administrator,** with a password and on the record. They keep
     what a person or a program deletes; a root shell can remove
     them too, as it can anything on the machine. The backup (6zm) is what lives elsewhere.
   - **What is deleted stays on the disk** until its snapshot goes, ten system changes later (`torq snapshots
     keep`), or an administrator deletes it (`request system snapshot delete`): a person
     who deletes a secret on purpose has not removed it from the machine, and it counts against the disk.
   - **Only system changes take one.** A person's own settings do not, or anyone could push everyone's
     restore points out without a password. So data lost between system changes may have no snapshot,
     and an administrator's ten changes in a row do move the oldest on, whoever's data they held.
   - **Folders of root's are not restored** (`/var/lib/systemd`, `/var/lib/private`, a service that runs
     as root): that takes break-glass. A folder holding several owners' files comes back as far as its
     owner can write it, all of it theirs.
   - **A restore is a copy.** Set-id bits are dropped on purpose; extended attributes and ACLs are not
     carried; it takes the space of what it brings back, and a person's `.restoring-` name beside the
     target while it runs. If a machine stops mid-copy that name stays, and nothing is ledgered.
   - **An owner can stall their own restore for as long as they like.** It blocks no one: the lock is
     not held. The administrator's command waits; there is no time limit, since a large copy takes hours.
   - **A device node in a folder fails the whole restore:** an owner cannot make one. What is live stays.
   - **Nothing holds a change off during a copy.** A commit confirmed may begin, or a rollback run, while
     data comes back; a prune may delete the snapshot being read, and the restore then fails, what is
     live untouched. The check for a waiting commit is at the start only.
   - **Parents a restore has to make are the owner's alone (700),** whatever they were in the snapshot, and
     stay after a failed copy: private, and to be opened up by hand. Restore the folder to get its modes.
   - **In a folder a person can write and does not own** (a 777 folder of a service's), they can swap
     in a link and the owner's copy follows it, as that owner. An administrator chooses that path.
   - **A service is not stopped for its data.** A person stops it first; the command says to restart it.
   - **A subvolume a person makes in their own home** is not in a snapshot of `/home`: its contents
     are not kept, and nothing says so.
   - **Machines on ext4 have none,** torq5 among them, so this is tested in VMs and not lived with.
   - **The image is not reproducible:** its btrfs and partition ids are random at build, and every
     copy of one image shares them. Two disks from one image on one machine are the case `not_elsewhere`
     already refuses to install into; booted, the first found wins.
   - **CI's disk room is unproved.** The image now takes a further stage to build; the first release
     build after this will say whether the runner has the space.
   - **A deleted snapshot is not wiped:** its blocks are free, not overwritten, so a secret in one is out of
     reach of the file system, not of someone reading the raw disk.
   - **Not built:** offering data at `rollback`. The declared backup and `request system restore`: 6zm.

6zl. **The audit of M3 (2026-11-06, `docs/AUDIT-2026-11-06.md`).** Fixed from it:
   - **H1:** a proposal's gate read the tier it was proposed with, while `apply` used what the diff did
     at HEAD. The socket refuses a proposal with no `--base`; `apply` refuses one that no longer does
     what was judged, ledgered as `apply_refused`.
   - **M1:** the push held the broker's lock across the network, and every request pushed: any account
     could keep every check busy, and so silent. The push has a lock of its own; a command that wrote
     nothing pushes nothing; `status` waits as an operation does, and says what the record says of
     itself while busy.
   - **M2:** any account could fill the one btrfs, and the ledger then had no room: `/var/lib/torq` is a
     file system of its own (1 GiB, root's). Machines installed from images before 0.2.2 never had btrfs.
   - **M3:** five wrong `su` attempts by anyone locked the owner out: `su` is root's alone.
   - **M4 and N19:** the agent could pad the ledger past what adopt takes: diffs and effects are kept
     beside it by hash, padding is refused, the agent's diffs are at most 1 MiB.
   - **M5:** a store path changed in place passed every check: a daily check, `repair` puts it back. The
     claim "cannot outlive a rebuild" is corrected (ADR-035).
   - **L1:** `show system alarms` names the remote's alarm. **L3:** the fence covers the socket and the PAM
     stacks. **M6:** tests for the rarest guards (see the commit log for each).
   Open from it:
   - **The store's own record of hashes** is root's to rewrite, and then the daily check agrees with it.
   - **What root leaves outside the configuration** survives a rebuild; only a reinstall clears it.
   - **unix_chkpwd**, setuid, checks a person's own password with no count: a program running as them can
     guess it there, slowly. The audit did not raise it; found while fixing M3.

6zm. **The declared backup (ADR-038, amended 2026-11-07).** Open:
   - **A repository's credentials have no protected place yet.** Only the passphrase is kept (`request system
     backup key`); the tree refuses rclone's keys and the key by another way, but an SFTP key or S3's keys in an
     `environmentFile` are placed by hand, root's, outside what TorqOS keeps, ledgers or backs up.
   - **Tested against a repository on the machine's own disk only** (`tests/backup.nix`).
   - **The key here is not the repository's.** Giving a new one changes what the machine reads, not the
     repository's own key (`restic key passwd`, by hand). A wrong one shows at the next run, and in
     `show system backups`.
   - **A backup that runs as another account is refused** (`user`): its key is root's alone.
   - **The room shares its disk with everything** (`/var/cache` is on the root file system): restic's own list
     of the path is summed first, and a restore that would not fit with a twentieth of the disk (4 GiB at
     most) to spare is refused. The sum is restic's word: a repository that lies about sizes can still fill
     the disk, as far as the restore's writes go, until it fails and the room is removed.
   - **A restore takes the room twice over while it runs:** restic writes the path whole into
     `/var/cache/torq/restoring`, and the owner's copy writes it again beside the target. A disk too full
     fails it, what is live untouched. A machine stopped mid-restore leaves that copy, root's, until the
     next restore clears it, and nothing ledgered.
   - **Everything 6zk says of a snapshot's restore holds** (it is the same copy): set-id bits dropped,
     extended attributes and ACLs not carried, root's folders not restored, a device node fails it, an
     owner can stall their half, nothing holds a change off during the copy.
   - **A path with `*`, `?`, `[`, `]` or `\` in it is not restored by itself:** restic would read it as a
     pattern. Restore the folder it is in.
   - **`latest` is the repository's newest,** from whichever machine: where several share one, name the
     snapshot.
   - **`show system backups` is anyone's,** and runs restic as root against the repository, a minute at
     most each: any account can make the machine reach a remote target. Its last run is systemd's, since
     boot; why it failed, the journal's, shown only to whoever may read the journal.
   - **Not built:** the restore step in setup's from-a-repository path (ADR-036), and a VM test against a
     remote target.
   - **Root's restic still writes as the repository says** (owners, modes, set-id bits) inside the room,
     confined to it (no device, no new privilege, the rest of the machine read only). The owner check
     before the copy and the copy as the owner are what keep that out of a person's folder.
   - **`extraOptions` reach root's shell as NixOS writes them,** unquoted (the review's L3, not taken):
     NixOS's own unit and wrapper write them so, and its example (`sftp.command='ssh …'`) relies on it;
     quoted in TorqOS's wrapper only, an option would mean one thing in the backup and another in the
     restore. Setting one is a system change, an administrator's.

6zn. **Doctor mode (ADR-042, built on `work/w3` 2026-11-07).** Fixed from its review (2026-11-07): the
   session's record is written before the first model call and after every tool result (a failed
   call left none); its id is random and made new (inside the launcher every pid is 2); the journal is
   off by default with any model; a tainted answer is headed so on standard output; plain http's host
   and port are compared whole and no endpoint has a user, query, fragment or backslash; llama-server
   has no network, its fixed flags last and the dangerous ones refused, a memory bound; each tool's
   program is bounded in bytes and time, at most 8 calls a round, curl's answer at most 4 MiB; the
   configuration taints too; invisible characters are cleaned. Open:
   - ~~**The taint record is the agent's own.**~~ Fixed on `work/w5` (2026-11-07, ADR-042 amended): the
     broker holds each session's taint in its own state directory (root's, 0600), sticky, ledgered; the
     doctor reports each tainting read there before the model sees it, and a proposal is judged by the
     broker's record. The record in the agent's home stays, as its account of itself only. What is
     left of it is 6zq's first item.
   - **The journal, once on, is the whole account's**: every program run as the agent reads every log,
     Claude Code included (which sends what it reads to its own service). A reader only the doctor has
     is later.
   - **With a remote endpoint the configuration leaves the machine**, a wifi network's key in it
     included, as well as anything else a tool returns.
   - **`tests/doctor.nix` has not been run** yet (amended on `work/w5` for the broker's sessions), and no real model has been tried: the unit tests and
     the VM test use a scripted endpoint. Whether a given local model calls tools well is untested.
   - **Anyone on the machine can use the local model** (its socket is 0666); restricting it to the
     agent's group needs the service off DynamicUser.
   - **llama-server runs from boot** once the local model is on, holding the model in memory; it does
     not start on demand.
   - **Only an administrator** can run `torq doctor` (it goes through `torq agent run`).
6zo. **`commit at` (broker/src/at.rs, ADR-043, 2026-11-07).** Fixed from its review: a run more than 15
   minutes late is refused (a machine asleep at its time, a clock stepped after boot, a broker busy
   all along), and the boot unit waits for `time-sync.target`; the approver is read again at the run (an
   account still, a person's, an administrator unless it is wholly their own folder); one bad schedule at
   boot no longer stops the others; one long past its time is settled by the next `commit at`; its
   approver may cancel their own; apply's own refusal of a proposal is not hidden by a schedule's.
   Open:
   - **`tests/commit-at.nix` is written and registered, not yet run** (the branch's worker may not build
     VM tests); the broker's unit tests stub the timer.
   - **A scheduled proposal is still listed** by `show system proposals`, and `request system apply` of
     it before its time applies it; the schedule is then refused at its time (already applied).
   - **Local times are read by `date`** when the commit is made (`HH:MM`, `"YYYY-MM-DD HH:MM"`): the
     broker holds UTC from then on, so a later change of the machine's time zone does not move it.
   - **Daylight saving.** A local time the clock skips (02:30 on the night it springs forward) is
     refused as no such time, if `date` refuses it — GNU `date` does. One the clock passes twice (01:30
     on the night it falls back) is whichever of the two `date` picks, and the person is shown the
     resolved time with the question only as a local stamp, not which of the two it is. `HH:MM` is
     today's or tomorrow's date at that wall time, so across a change it is still that wall time (not 24
     hours on). The time shown in `show system commit` uses the offset at that moment, read by `date`.
   - **The grace is fixed** at 15 minutes (broker/src/at.rs `LATE_MINUTES`), not a setting.
   - **A timer armed and never recorded** (the broker killed between the two) waits for its time, finds
     no schedule by its id and does nothing; it is not removed before then.
   - **A run that fails before it can record anything** (the ledger or accounts unreadable) retries for
     25 minutes and then stops; the schedule is settled on the record by the next `commit at`, or seen
     waiting, marked past its time, in `show system commit`.
   - **Adopted with one waiting:** a machine adopting a repository whose ledger holds a schedule arms it
     at its first boot; adopt's own commit moves HEAD, so it is refused at its time, not applied.
6zp. **The socket's and the ledger's words are written twice (code review, 2026-11-07; ADR-044, after
   0.3.0).** `cli/src/run.rs` re-declares `Request`, `Frame`, `SOCKET`, `NEED_PASSWORD` from
   `broker/src/socket.rs`, and reads exit codes 1, 2, 75 as bare numbers; `cli/src/shell.rs`'s
   `ledgered_commit` and `waiting_proposals` repeat the broker's kind lists (`broker/src/lib.rs`); entry
   kinds, tiers and actors are strings matched across broker/, cli/ and agent/; a commit id, a store path and
   a snapshot label are checked ad hoc (`repo::revision`, `lib.rs` `shapes_ok`, `snapshot::label_ok`).
   No drift today: the two kind lists match, and `tests/socket.nix` exercises both ends of the protocol. A
   kind added on one side only would go unnoticed by the compiler. ADR-044's crate `torq-wire` closes it,
   fixtures first so the ledger's bytes are proven unchanged; not in 0.3.0 (it changes all three Nix
   packages' sources and touches `lib.rs` while the release's branches do).

6zr. **Python out of a new machine (ADR-032/033, amended 2026-11-07, `work/w6`).** The root is grown by
   systemd-repart in the initrd, not growpart; the SSH greeting runs fastfetch's program without its
   Python completions. Open:
   - **Python stays for the boot writer** (systemd-boot's installer, ~135 MB): ADR-032's amendment says
     what replacing it would take. The size budget stays 1280 MB until then (1252 MB now).
   - **Not yet run in a VM:** `tests/installer.nix` (repart's definitions compared instead of
     `boot.growPartition`, and the layout check; repart's "Growing existing partition 2." in the journal,
     its numbering from 0, and nothing of partitions 0 or 1; the ESP's and the record's entries as the
     image left them, the record's of TorqOS's type; no growpart unit) and `tests/image.nix` (which boots the
     image, grown, and was not changed). What only they show: that repart in the initrd finds the
     disk behind a btrfs root, and that btrfs then grows on `subvol=@`.
   - **Machines installed before this** keep `boot.growPartition` in their own hardware file, and with
     it cloud-utils and Python. Their disks are not the image's layout now (no partition names, the
     record of Linux's generic type), so the new hardware file's repart would not run on them: its
     layout check skips it. Theirs stay grown by growpart, which does nothing once grown.
   - **Only the image's layout grows.** Any other (an install by hand, a partition added) is left as it
     is, and its root is not grown: by design, never repartitioned rather than guessed at.
6zq. **Proposals and session trust (ADR-042 6 and 7, amended; built on `work/w5` 2026-11-07).** Open:
   - **A session's taint is what was reported.** The broker cannot see what a program of the agent's
     reads; it holds what it is told, where the agent cannot undo it. TorqOS's doctor reports every
     tainting read before the model sees it, and stops if a report fails. But any program run as
     `torq-agent` (Claude Code under `torq agent run`) can start a session over the socket and never
     report, so its proposals look untainted — and under a person's trust, a user-tier change in that
     person's folder (their packages) would then apply without asking. Same-uid programs cannot be
     told apart: checking the caller's executable (ADR-042, 7 as first designed) is defeated by a
     parent that traces TorqOS's own doctor. What bounds it: trust is the person's own folder, at
     most 60 minutes, given with their password, each change ledgered and revertible; never system
     tier. A real fix would run TorqOS's own agent as an account other tools cannot run as.
   - **Expiry is ledgered late.** A grant is over at its time whatever the ledger says; its
     `trust_expired` is written by the next operation that may apply under trust (a proposal, a grant,
     a revoke), not at the time itself.
   - **What counts as cutting the machine off** is a fixed list (broker/src/agent.rs `CUTS_OFF`, widened
     by the review of w5): the network and the ways in over it (networking, systemd.network, ssh and
     its alias `services.sshd`, sshguard, resolved, tailscale, a person's `openssh` keys under
     `users.users` and `users.extraUsers`), what runs (`systemd.services`, `environment.etc`),
     hardware and boot, the screen and the console (console, getty, kmscon, the display's services,
     niri). Not a setting, and only for the agent's proposals: a person's own commit is theirs to
     confirm or not. A setting not on it that a network or a login still depends on (another VPN's,
     a `systemd.sockets` or `systemd.user` unit, `security.pam`) is not caught; NixOS's other aliases
     of a listed option are caught only where listed.
   - **`tests/agent-proposals.nix` is written and registered, not yet run.** No VM test runs the
     doctor's own propose step (torqcli's commit as the agent evaluates the configuration, which the
     test VM cannot): it is covered by the agent crate's tests, with torqcli stubbed.
   - **Sessions share one budget.** At most 600 are started an hour and 2000 records kept (the oldest
     go; a proposal naming a gone one is refused). The doctor's and any other agent program's are one
     account's, so no separate budget by uid: a program run as the agent can keep the doctor from
     starting a session for the rest of an hour — it is refused then, never less careful. The ledger
     holds a session only in the proposals that cite it.
   - **A trusted apply already started finishes** after a revoke: the revoke is on the record at once
     (the sessions' own lock), and every later trusted apply reads it, but one in its build goes on.
   - **The doctor's values are words**: `propose_change` sets one word, number or boolean per setting;
     a value with spaces or quotes (a description, a list at once) cannot be proposed.

6zs. **The audit of 0.3.0 (2026-11-08).** Its report was cut short; its tests were kept
   (broker/tests/audit_030.rs), and each failed against the candidate. Fixed from it:
   - **A2 went around twice:** a flake.lock padded past 1 MiB was read as no lock, so nothing in it was
     checked; and an input given the type `path` was skipped, whatever store path it named. A lock too
     large is refused, and a folder is hashed again like any input.
   - **One session's record could stop every session:** a thousand sources reported grew it past what
     the broker reads. A session names at most 32; past them it is tainted all the same.
   - **A grant lasted as long as its `until` said:** it lasts its minutes from its own entry, at most 60.
   - **Adopt took a ledger with a trust grant standing or a commit at waiting:** refused.
   - **The doctor's limits** are tested as numbers.
6zt. **One check per commit, the install question, `| display detail` (torqcli, `work/w8`,
   2026-11-08).** A commit evaluates the configuration once (docs/CLI-DESIGN.md, "The commit
   model"); `commit check` then `commit` once in all; `request system update` asks to install. Open:
   - **Kept results are the person's own files:** SSH before a change (`~/.cache/torq`) and a
     check's result (in the candidate) are trusted as the candidate itself is — whoever can write
     them can already change what that person commits. A tampered or stale one can make the review
     show less: SSH off after the change (a reused check's) makes the SSH review return early, so
     the refusal of a change that locks everyone out (keys only, none) is skipped, and so is the
     refusal of tree files the configuration does not read (`reads_tree`); a key list or the
     passwords notice can be left out. The same-user trust model holds — it is that person's own
     approval of their own candidate — and the broker builds the commit itself. Kept results are
     named by a hash of the Nix that read them, so another torqcli's are not reused.
   - **The cache folder** is held open and checked (this user's, 0700, not a link) before each
     entry is read or written by name in it; `~/.cache` above it is the person's own and is not
     checked (a link there points into their own files, or nowhere they could not already write).
   - **Not yet seen at a real terminal or in a VM test:** the live output of `| display detail`
     (Nix's and the broker's lines, the password asked between them) and the install question are
     tested through the fake machine only; the one `nix eval` was run against this repository's
     dev-vm configuration by hand. tests/trust-loop.nix could run `commit | display detail` and an
     update answered at a terminal.
   - **What the broker streams** during its build is what it sends as frames: if it sends its build
     output only at the end, detail shows it then.

6zu. **A first boot with no network (ADR-048 5, built 2026-11-09, `tests/installer.nix`).** Open:
   - **x86_64 only.** The first configurations are x86 machines (an AMD and an Intel CPU, their
     microcode); an arm64 installer carries nothing for its first build, which fetches.
   - **Evaluating the x86 installer needs an x86 builder:** the carried set is read from the .drv files
     at build time and imported (`hosts/installer/first-build.nix`). CI and the dev host are x86_64; an
     arm64 machine evaluating `nixosConfigurations.installer-x86_64-linux` would need a remote builder.
   - **Covered by its samples, proved on one machine:** what differs between two kinds of machine (AMD
     with Wi-Fi and SSH by key; Intel with neither) and two machines of each. A machine whose hardware
     brings what none of them has (an out-of-tree driver, `boot.extraModulePackages`) needs the
     network at its first build; setup does not yet say so before it starts (ADR-048 5's message).
     The VM test builds change #1 for a machine none of the samples is (a VM, no firmware): 23
     derivations, from the disk alone. Real hardware (firmware, iwd) is proved on the Zenbook.
   - **Setup's own path is not what the test runs:** the test loads the registration as setup does
     (`register_carried`) and builds as the broker would; tests/image.nix's setup still switches to a
     prebuilt specialisation.
