# The configuration is the device (proposal)

Status: **built** (S1–S5, 2026-10-31): proposed first, then built step by step as below (the focus of TorqOS
since ADR-019). Becomes ADR-018 once
agreed.

## The idea

As on a router, one configuration defines the whole machine: what software it runs, every system
setting, every service, every user and what they may do. You change the machine only by changing
the configuration (`configure`, `set …`, `show | compare`, `commit`), so the machine is always
exactly what its configuration says, every change is recorded, and any change can be undone.

What the configuration does **not** hold is data: people's files and what applications save for
themselves (home folders), and what services accumulate (databases, container images, logs in
`/var`). A commit or a rollback never touches data.

| | Owned by | Changed with | Survives commit / rollback |
|---|---|---|---|
| Everything outside home folders: software, `/etc`, services, users, groups, boot | the configuration | torqcli (`commit`) | it *is* the configuration |
| Home folders: files, app settings saved by apps | each user | anything they like | never touched |
| Service data: databases, containers, logs | the services | the services | never touched |

## The tree: NixOS's own settings, one to one

Every NixOS setting is a path in the tree, dots becoming spaces. NixOS describes all of its
settings (name, type, default, description) in machine-readable form, so the tree is generated
from the machine's own NixOS version — TorqOS's settings included — and `?`, Tab, help and type
checking work for all of them, with nothing written by hand per service.

```
virtualisation.docker.enable = true;                        virtualisation docker enable true
services.httpd.enable = true;                               services httpd enable true
services.httpd.virtualHosts."example.com".forceSSL = true;  services httpd virtualHosts example.com forceSSL true
environment.systemPackages = [ pkgs.htop ];                 environment systemPackages [ htop ]
networking.firewall.allowedTCPPorts = [ 80 443 ];           networking firewall allowedTCPPorts [ 80 443 ]
```

| NixOS type | In the tree |
|---|---|
| true / false | `… enable true` (a bare `… enable` is `true`) |
| string, number, path, one of a list | a value; `?` lists the choices |
| list of values | `[ a b c ]`; `set` adds, `delete` removes one |
| named entries (`virtualHosts."example.com"`, `users.users.viper`) | list entries, as `users viper` is today |
| a package | its name (`htop`), checked against this machine's nixpkgs |
| multi-line text (`extraConfig`) | a quoted value, or a file (below) |
| Nix code (functions, expressions) | not a value: a `config` file (below) |

So any NixOS guide or answer applies directly; nothing new to learn beyond the translation.

**Names and quoting.** Nix quotes `"example.com"` only because dots separate its setting names; in
the tree spaces do, so `example.com` is one word, and under a level of named entries (`virtualHosts`,
`users`) any word is an entry name. Quotes are for spaces and the shell's own characters
(`"My Site"`, a value with `|` or `;`), shell-style, as torqcli reads them today.

**No variables, at first.** Every value is literal, so `show | compare` and an approval prompt say
exactly what changes — nobody approves a puzzle, least of all an AI's proposal. When repetition
hurts, J style `groups` (a block applied in many places, `apply-groups`, with
`| display inheritance` to show the result expanded) cover most of what variables are used for.
Computed values and references between settings belong in a `config` file: plain Nix.

## Files for what is not a value

At any point in the tree, `config <file>` attaches an ordinary NixOS module for that part:

```
systemd services myapp config ./custom_apps/myapp.nix
services httpd config ./services/httpd.nix
```

- The file is plain NixOS: everything NixOS can do works, including what the tree cannot express.
- It must be inside the config repo, so it is versioned, ledgered and rolled back with the rest. A
  path outside the repo would make the machine depend on something nothing tracks: refused.
- Humans edit it through the candidate: `edit services httpd config` opens it in your editor, and
  `show | compare` shows its raw diff before `commit` — the same flexibility, plus undo.
- `show configuration` names every attached file, so nothing that defines the machine is hidden.

## TorqOS's layer on top

**One tree, all generated.** NixOS already has top-level `system.*`, `services.*` and `users.*`, the
names TorqOS's first hand-made tree used. So nothing is hand-made: the whole tree is generated from
the machine's NixOS options, and TorqOS's own features are NixOS options under `torq.*` (as
`torq.user` and `torq.dev.enable` already are), shown as `torq …`. Today's names become their NixOS
equivalents (`system host-name` → `networking hostName`, `system packages` → `environment
systemPackages`, `services ssh` → `services openssh …` with TorqOS's defaults); a few short
aliases may stay where they help. NixOS's documentation file describes types in words, so the
generator reads NixOS's type objects and writes its own structured file (measured: 24,603 settings
on this machine, generated in ~6 s; the documentation file is 12 MB).


A few TorqOS branches add safety and ease where raw NixOS is awkward; they set NixOS settings
underneath and show what they set (`| display detail`).

```
system host-name torq-utm                     (networking hostName)
user alice applications [ htop cowsay ]       (that user's own programs)
user alice group [ admin ]
user group admin system read-write            (may approve system changes)
user group users1 system read                 (sees the configuration; commits only their own folder)
user group users1 applications [ htop ]
```

- **Applications vs services** are told apart from NixOS itself: a service is a NixOS `services.*`
  (or `virtualisation.*`, `systemd.services.*`) module that runs for the machine; an application
  is a package a person runs. `?` offers each where it belongs.
- **TorqOS defaults stay defaults** (SSH keys only, no root login, firewall on): an explicit
  setting wins, as in NixOS, but `commit` says plainly when one weakens a TorqOS default ("this
  allows password logins over SSH").
- An application's own settings are either its state (what it saves when you click, as today) or,
  per user and per application, a declared file (`user alice applications htop config …`, using
  home-manager, NixOS's per-user tool).

## Humans free, the AI fenced — permanently

**The fence (built, S3a).** modules/settings-fence.nix: the tree holds data only (a key starting with
`_`, NixOS's module directives, is refused anywhere), and the broker's own inputs — its commands and
paths, what its settings file and PATH are built from (`system.build`, `nix.package`,
`systemd.package`), its units, its files in /etc by where they land, `nixpkgs` — are not the
tree's. Enforced when the system is evaluated. It is not the AI's boundary (KNOWN-GAPS 6m): settings
that run root code stay settable for an approving person; an agent gets a default-deny allow-list.

- The AI runs as **its own account**, sandboxed, without `sudo`. It cannot write the configuration
  or any system file; it can only *propose* a change to the broker.
- The broker learns who is calling from the kernel (not from anything the caller says) and applies
  a policy to AI proposals: whole areas it may never propose (users, groups, permissions, boot,
  security, the broker and its policy), and a person approves every one it may.
- Humans keep full power, root included; nothing here limits them. The read-only system files are
  about determinism (the machine always matches its configuration), not about the AI.
- This closes KNOWN-GAPS #4 (today, anything running in your terminal acts as you). Planned for M3;
  nothing above has to wait for it.

## What the config repo becomes

Data files for the tree, `flake.lock`, and any attached `config` files. `flake.nix`, `machine.nix`
and `system.nix` become fixed TorqOS files nobody edits. Hardware (`hardware-configuration.nix`,
really data: disks, kernel modules) becomes a `hardware` branch, detected at install. Software
sources (the flake's inputs) become `system software …`, like choosing a firmware train. Always
deterministic: the same configuration and the same lock build the same system.

This machine today, moved over without changing what it does:

```
torq.user = "viper";              →  user viper group [ admin ]      (and the machine's owner)
torq.dev.enable = true;           →  torq dev enable true            (visible, and flagged)
services.openssh.enable = true;   →  services openssh enable true    (TorqOS's protections: keys only)
torq.desktop.enable = true;       →  torq desktop enable true
```

## Open questions

1. **Size.** NixOS has ~20,000 settings; the generated description is tens of MB. Measure the
   cost of building it with the system and of torqcli reading it.
2. **How far the AI's fences go**, area by area.
3. ~~**Today's `services ssh` catalog**: keep as a TorqOS shortcut, or fold into `services openssh`
   with TorqOS's defaults and warnings.~~ Decided (ADR-025 #6): folded in. Services are NixOS's own
   (`set services openssh enable true`), with TorqOS's protections applied however they are turned on
   (modules/services.nix); the old catalog keeps working on machines that have it, with a warning
   saying what to move to, and torqcli never adds to it.
4. **Unmanaged mode**: whether anyone needs to edit the repo outside torqcli at all, given
   `config` files and `edit`.

## How it would be built (each step reviewed, tested, shipped through the broker)

1. Generate the tree's schema from the machine's NixOS options, installed with the system; read it
   in torqcli (help, completion, types). **Built (S1):** modules/nixos-schema.nix writes
   /etc/torq/nixos-schema.json (all 24,603 settings here, 4.4 MB, ~9 s more per system build);
   cli/src/nixos.rs reads it (17 ms), resolves paths with named entries, checks values against
   NixOS's types, offers what may come next. Not yet editable in `configure` (S3).
2. One generic module applies the tree's values to NixOS; values stored as data in the repo.
   **Built (S2):** `nixos/<top-level>.json` in the config repo, shaped like the options
   (`nixos/virtualisation.json`: `{ "docker": { "enable": true } }`); modules/nixos-settings.nix
   gives them to NixOS as they are (package names become packages), built by `mkSystem` from the
   path itself; NixOS's types check every value. All system tier at the broker.
**Built (S3a–S3c):** the fence; settings.rs (the nixos/ files) and whole.rs (one tree of NixOS paths
   over both kinds of files, the mapping table, old names as aliases); `configure` — set, delete,
   edit, show, compare, `| display set|detail`, `?` and Tab — on it; the SSH key check by NixOS's
   path too; reserved words refused as entry names.
**Built (S3d):** this machine's owner, dev mode, SSH and desktop moved from system.nix/desktop.nix
   into nixos/torq.json and nixos/services.json (ledger #35); the migrated repo built the very
   system that was running (same store path), so it changed nothing but what is visible.
3. `config <file>` and `edit`, through the candidate and `commit`.
   **Built (S3e):** `set <place> config <file>` attaches an ordinary NixOS module file of the config
   repo (a `.nix` file inside it: relative, no `..`; not TorqOS's own flake.nix)
   at any place of the tree, kept in `config-files.json` and imported by modules/nixos-settings.nix.
   The place is where it is shown and found; the file is a whole NixOS module, and may set anything
   (system tier; KNOWN-GAPS 6m). `file edit <file>` opens your `$EDITOR` (`$VISUAL`, else nano or
   vi) in configuration mode, and the result waits in the candidate; `file show`, `file list` (where
   each is attached, what the candidate changed), `file delete` (refused while attached; `delete
   <place> config` unattaches). `show | compare` shows each edited file as its raw diff, under
   `[edit config files]`, and that is what a person approves; a commit touching any Nix file has
   its SSH state evaluated like any other. Where NixOS has its own `config` (`containers <name>
   config`) or below named entries, the word is NixOS's. An attached file is part of the
   configuration: `show configuration` no longer lists it as set outside it.
4. ~~The TorqOS layer (shorthand for users, groups, applications)~~: dropped (2026-10-31): the tree
   uses NixOS's own paths (`users users <name> extraGroups [ … ]`). TorqOS-default warnings at
   commit may still come.
5. `hardware` and `system software` branches; fixed `flake.nix`/`machine.nix`; migrate this machine.
   **Built (S4a):** hardware is not a branch of its own: it is NixOS's own paths (`fileSystems /
   device …`, `boot loader systemd-boot enable true`), like every other setting. `flake.nix` is
   fixed (TorqOS's own, no modules; `file list` says so, `show configuration` no longer lists it as
   set outside). `machine.nix`, `system.nix` and `desktop.nix` are gone from the template, which is
   data plus the `hardware-configuration.nix` nixos-generate-config writes at setup, attached at the
   top of the tree (`config hardware-configuration.nix;`) until an installer writes its values into
   the tree (M2). It imports nixpkgs profiles (qemu-guest), which are not values and should keep
   following nixpkgs. This machine is migrated the same way: its machine.nix values (boot loader,
   the qemuGuest and spice services) go to nixos/boot.json and nixos/services.json, its
   hardware-configuration.nix is attached at the top, and it builds the very system that was
   running (ledger #46; `show configuration` no longer names anything set outside it). tests/tree.nix evaluates the template as a whole machine. A change to how the machine
   starts is warned about at commit (KNOWN-GAPS 6n). Also fixed: `| display set` prints whole lines
   from the top (pasteable); `torqcli … | head` stops quietly (141), and a failure to
   write the output (a full disk) is an error; a top-level attachment (`set config x.nix`) reads
   back.
   **Built (S4b):** `system software torq source <source>`: where TorqOS comes from, the one
   `torq.url` line of the fixed flake.nix, shown in the tree (`system { software { torq { source …;
   } } }`). Setting it locks the new source at once, into the candidate (`show | compare` shows the
   source and the new software together; `commit` writes both, system tier); `request system
   update` then updates that source; `request system software rollback` is refused while the
   candidate changes the source; it is never deleted. Only plain flake references (`github:`,
   `gitlab:`, `sourcehut:`, `git+https://`, `https://`, `path:/`), written with plain URL
   characters. The version stays the lock's (`## software: …`, `request system update`), as network devices
   keep configuration and software installs apart. A flake.nix that is not TorqOS's fixed one has
   no source in the tree. This completes step 5: nothing of the machine is outside the tree.
   From the independent review: `rollback <n>` dropped the source but kept its lock, so a commit
   could pair the old flake.nix with another source's lock, and Nix would quietly re-lock it, as
   root, to something nobody saw (proven on a scratch copy). Now a lock never outlives its source
   (`rollback <n>` drops it), and the broker builds, and `commit check` evaluates, with
   `--no-update-lock-file`: a flake.lock that does not match flake.nix fails, never re-locked. A
   folder as the source (`path:`) is said in the commit question. Live as change #48; the broker's
   first build with the flag was the next update.
6. `save` / `load override|merge|set`: a saved configuration recreates a device.
   **Built (S5):** `save <file>` (configuration mode) writes the whole candidate, from the top, in
   the brace format `show` prints, and then the content of every Nix file of the configuration
   (attached or imported by one; `## config file <path>`, each line `##| …`, byte for byte), so the
   file alone recreates the device; written private (0600, a new file renamed into place: never
   through a link, never over a device), as a configuration can hold secrets. `load override <file>` makes the candidate exactly
   the file (its settings, attachments, config files' content, the software's source, which is
   locked as `set` locks it; software already loaded into the candidate for the same source, and
   Nix files the file does not name, stay); `load merge <file>` sets the file's settings onto the candidate; `load
   set <file>` runs its `set` and `delete` lines (what `| display set` writes, from the top) and
   nothing else, so a file can never commit. All or nothing: one refused statement and the
   candidate is as it was, the error naming the line (all or nothing for errors: `load set` keeps
   the candidate as it goes, so a crash midway leaves it partly loaded, as a paste would). Nothing reaches the machine until `commit`
   shows it and a person approves it. The software's version is not saved, only its source (as
   network devices keep configuration and software apart): a loaded configuration builds on the newest
   (or, for the same source, the candidate's already loaded)
   software of that source. On other hardware, the saved `fileSystems`, `boot` and hardware file
   are this machine's: the commit question warns about them (KNOWN-GAPS 6n). Live as change #52:
   this machine saved (80 lines, its hardware file's content in it) and loaded back with `load
   override` is the identical candidate.
