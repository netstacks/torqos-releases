| `show users` | the people with an account here (NixOS's isNormalUser: numbers 1000 to 29999), where each is logged in now, and their groups; it says where groups are set (`users users <name> extraGroups`) |
# CLI design — `torqcli`, the TorqOS shell

Status: **plan** (milestone M1.5 in `ROADMAP.md`; ADR-016). TorqOS is driven like a network
device: a shell with an operational mode and a configuration mode, a private *candidate*
configuration, `show | compare`, `commit check`, `commit confirmed`, `rollback <n>`. One syntax, the
J style (`set`, `delete`, `edit`, `show | compare`; ADR-021). Anyone who has run a router already
knows how to drive TorqOS.

## Principles

1. **One model, one syntax.** Every command is one operation (`Op`); the J style is the only way to
   type it (ADR-021: one clean language, its help and errors in one voice).
2. **The configuration is a tree, and the tree is the config repo's files.** Every setting has a
   path; `show configuration` renders the repo; a commit is a diff of files. Settings are declared in
   a schema (TorqOS's YANG), which also drives `?` help, Tab completion and syntax checks.
3. **Show the change in the user's terms.** `show | compare` shows packages, services, variables —
   with versions and descriptions where useful — never a JSON or `flake.lock` hunk (`| display
   diff` shows the raw change).
4. **Quiet while working.** One spinner line naming the phase and the elapsed time; the full log in
   `~/.local/state/torq/last.log`, shown on failure.
5. **No internals unless asked.** Hashes, store paths, ledger kinds and broker commands appear only
   with `| display detail`.
6. **Commit is the approval, and it says exactly what it approves.** `commit` prints the change it is
   making before it switches; any `.nix` file in it is shown as its raw diff. A system-tier commit
   needs a terminal — which stops cron, services and other background jobs, but **not** anything
   started from your terminal (an AI tool, a script, a package's install hook): until the broker
   identifies its caller (M3, KNOWN-GAPS #4), whatever runs in your terminal can commit as you.
7. **Errors say what, why and what to do** — never `Caused by:`, git hints or Rust error text.
8. **One visual language.** Colour with meaning only; `NO_COLOR` and non-terminals get plain text;
   fits 80 columns.

## The shell

```
$ torqcli                                   # interactive
viper@torq-utm> show system alarms          # operational mode
viper@torq-utm> configure                   # configuration mode
$ torqcli show system alarms                # one command, for scripts
$ torqcli -c "configure; set users users viper packages htop; commit"
```

| | |
|---|---|
| operational prompt | `viper@torq-utm>` |
| enter configuration | `configure` (or `config`); `configure exclusive`: the shared candidate yours alone until you leave |
| configuration prompt | `[edit networking]` line, then `viper@torq-utm#` |
| add / change | `set environment systemPackages jq` |
| remove | `delete environment systemPackages jq` (one value); `delete environment systemPackages`, `delete networking` (everything below); not set: a warning |
| go down / up / to the top | `edit networking` · `up` (or `exit`) · `top` |
| compare with the active configuration | `show \| compare` (`show \| compare rollback <n>`: with an earlier one) |
| show the configuration | `show` (candidate) / `show configuration` (operational) |
| run an operational command | `run <command>` |
| leave configuration | `exit` at the top, `exit configuration-mode`, or Ctrl-D from anywhere (it leaves configuration mode, not torqcli) |
| help | `help`, `help <command>`, `help <words>` |

`up` goes one level at a time. A set of named entries is a level of its own (`services httpd
virtualHosts`, as a network device's `interfaces` is above `ge-0/0/0`): `up` from an entry goes into
its set, where entries can be set and shown. `up ?` offers `<n>`.

A change to who can log in over SSH (read from the evaluated configurations before and after it,
so every way counts: turning it on, allowing passwords or turning them off, a key removed, another
owner) shows at `commit` exactly which keys will be able to log in, as the changed configuration has
them, or that passwords will be accepted. If SSH lets in keys only and there are none, the commit is
refused (nobody could log in), saying to add a key or allow passwords — whether SSH turns on with it
or was on already. `request system revert` checks the same, with the same function
(`session::ssh_review`).

And: `commit`, `commit check`, `commit confirmed <minutes>`, `commit at <time>`, `commit comment "<text>"`, `commit
and-quit`, `rollback <n> [<place>]`, `up [<n>]`, `top [<command>]` (a command from the top, then
back where you were — unless it moves), `show system commit`, `?` help, Tab completion, history (↑).
`?` lists what may come next, `|` after any whole `show`, and `<[Enter]>` when the line would run as
it is (decided as the session decides it: the line is dry-run on a copy of the candidate).
Inside quotes `?` is a character: an address with a query is typed `"git+https://…?ref=stable"`
(one typed without quotes loses its `?`, and a source that did is refused, with the quoted address).

**One grammar** (cli/src/grammar.rs, ADR-025 #1): the command language is declared once — each
command's words, help, values (checked, with their range), the pipes its output takes, and how a
whole line becomes an operation. One walk over it parses a line, expands abbreviations, lists what may
come next for `?` and Tab, answers `help`, and says why a line is refused, so these cannot disagree.

- A word that begins exactly one word that may stand there is that word (`sh conf | gr host` is
  `show configuration | grep host`; pipe words too); one that begins several is ambiguous, naming
  them; one that begins none is named, with what may stand there, and a word one letter off is
  suggested ("did you mean configuration?").
- Every refusal says why, in one form, naming the line once: what may follow (`show system`), the
  value needed (`needs <change> (…)`), that nothing may follow, a value out of range (`'2000' is out
  of range: minutes go from 1 to 1440`).
- A command of the other mode says so, wherever it stands ("'edit' is a configuration-mode command
  (after `file`): `configure` first").
- Other CLIs' words and paths are one table (`cli/src/vocabulary.rs`), read wherever one is
  refused and by `help`: `| count` → `| wc -l`, `| match` and `| find` → `| grep`, `| except` →
  `| grep -v`, `| last` → `| tail -n`, `| trim` → `| cut`, `| save` → `save <file>`; `copy`,
  `rename`, `annotate`, `deactivate` say what TorqOS does instead; `ping`, `traceroute`, `ssh` are
  shell commands, run outside torqcli. Another CLI's path says where the setting is here and how it
  is set: `set system services ssh` → `services openssh`, `set interfaces …` → `networking
  interfaces`, `system name-server`, `ntp`, `login`, `syslog`, `routing-options`, `firewall` (an old
  name that works here, `system host-name`, needs none).
- `help` lists the mode's commands; `help <command>` says what it is and what may follow it
  (abbreviated as anywhere: `help sh sys`); `help <words>` searches the commands of both modes and
  the settings: first where the concept usually is here ("On TorqOS:", from the table above), then
  places (a service, a section: `virtualisation docker`), then single settings — each closest
  first: a word as a whole part of a name (`hostname` is hostName), its beginning, inside one word
  (`ssh` in openssh; never across two: `mountPoint` is *mount* and *point*), then in the help.

Configuration paths (old names, entries people name) and values are not the grammar's: the tree reads
them, with the setting's own rules (below). `?` offers only what can be set: not NixOS's `_module`,
nor read-only settings, nor what the fence keeps out. Where a value is a file, `?` and Tab offer the
configuration's Nix files (`file show|edit|delete <nix-file>`); `save` and `load` take any `<file>`.

**Pipes**: `| compare`, and one `| display`: `set` (a configuration as `set` commands), `detail` (what
is behind a view: the config repo files and commit of a configuration, the commit ids of `show system
commit`, the broker's commits and store paths in `show system alarms`), `diff` (a comparison as the
raw diff of the files: for the candidate exactly what `commit` proposes; between two commits, the
files as committed). Each view refuses what it does not offer. Then any
number of **filters**, the real Linux tools with their own options (ADR-017): `| grep`, `| wc`, `| head`,
`| tail`, `| sort`, `| uniq`, `| cut`, `| tr`, after any `show` — e.g.
`show configuration | grep -i host`, `show system commit | head -n 5 | wc -l`. They read the show's
output inside a sandbox that has nothing else (no files, no network), from the system's own
coreutils and grep; words are split as a shell does (`'single'` quotes are literal). `help` and
`file show` take filters too. A filter that cannot run without something says so (`| grep needs
<pattern>, as in | grep host`; `| grep ?` offers `<pattern>`); a tool's own error is said in its
name (`| grep: missing closing parenthesis`).

**The pager.** Output taller than the window, from a line typed at the prompt, is shown a screenful
at a time, as on a network device: `---(more N%)---`, Space for the next screenful, Enter for one
more line, `n` for the rest at once, `q` (or Ctrl-C) to stop, `h` for the keys; any other key is
ignored (the rest is not lost to a slip). Lines that wrap count
as the rows they take. Never for a script, piped output, or a paste's later lines (they come at once;
the first line of a paste may page, the rest wait for it). `| no-more`, anywhere among the pipes,
shows it all at once.

**One grammar.** Every line parses to one operation; a property test renders every operation as it
is typed and parses it back. `config` is reserved: it can never name a list entry, since `<place>
config <file>` attaches a file.

**Delete** works as on a network device (ADR-022): `delete <setting> <value>` takes that value of a
list; `delete <setting>` or `delete <place>` takes everything below it — `show | compare` and
`commit` show exactly what goes before anything does. What is not set is a warning (`! nothing
deleted: … is not set`), not an error, so a paste goes on. `delete` with nothing after it takes
everything at this level, and asks first.

**Rules between settings** (a `path:` source needs `torq dev enable true`) are checked on the whole
configuration, never line by line, where their order would decide: a change that breaks one is
said at once (`! … — commit refuses until then`), and `commit` refuses until it holds.

The line editor's `?` and Tab follow where you are. In a `?` list each help is its setting's first
sentence (NixOS's doc markup taken out, `…` where it was cut), at most three lines (see "Layout"
below).

**Every command says what it did** (ADR-025 #5). `rollback` says what it loaded or discarded,
counting changes as settings (a changed value is one change): "Loaded configuration 3: 2 changes from
the configuration in effect", "Discarded 1 change", "No changes to discard". `show | compare` with
nothing pending, two configurations with the same settings, and `show` of a place with nothing set
say so. Every way out says what is kept: `configure` with changes left from before says so (and how
to list or discard them); `exit` from torqcli keeps the candidate and says so; Ctrl-D in
configuration mode leaves it as `exit configuration-mode` does, and in operational mode leaves
torqcli; `-c` ends saying its uncommitted changes are discarded. `save` and `load` read a
relative file name as in the home folder, as on a network device, wherever torqcli was started; `save`
says the full path.

**Scripts (`torqcli -c`)** use a private candidate that exists only for that invocation — never your
interactive candidate, so a script cannot commit your half-done edits or leave edits behind. They
stop at the first command that fails, saying how many after it did not run, and quote
values containing `;` or `|` with double quotes. `torqcli --help` prints how torqcli is run and
exits 0. `torqcli <command>` runs one operational command
the same way (`torqcli show system alarms`, `torqcli show configuration '|' grep host`): its words as
your shell split them, or one argument read as a whole line (`torqcli "show configuration | grep
host"`); its exit code says whether it worked. Configuration needs `-c`. Interactive sessions of the same user share one
candidate (as `configure` on a network device); `configure private` is for later.

**`configure exclusive`** (built, cli/src/exclusive.rs): configuration mode with the shared candidate
held by this session alone, until it leaves configuration mode (`exit`, `exit configuration-mode`,
Ctrl-D, `commit and-quit`) or torqcli ends. It says so on entering, and on leaving that it let go.
The person's other interactive sessions can enter configuration mode and read the candidate — as the
holder saved it, re-read at each command — but every command that would change it (`set`, `delete`,
`rollback`, `load`, `file edit|delete`, `request system update`, `software rollback`, `pull`) or
commit it is refused, naming the holder (`torqcli process 4242 on pts/3, since 2026-11-07 14:02:51`);
`configure` there says the same, and `configure exclusive` is refused. `commit check`, and a `commit`
of nothing (confirming a commit confirmed: the machine's, not the candidate's), are not refused. The
lock is the kernel's (`flock` on `candidate.lock` beside the candidate, in the same private folder),
so it goes with the process that took it: a session killed holding it leaves only what it wrote,
which the next `configure exclusive` finds free and takes over, saying whose it was ("A lock left by
a session that ended without leaving configuration mode (…, gone) was taken over"). A reused
process number cannot keep a lock. A script's private candidate (`-c`) is never locked, nor stopped
by a lock (`configure exclusive` there says it is private already). Unlike some network devices,
leaving keeps uncommitted changes, as `configure` does.

The lock is checked again at every write, not only when a command starts, since a command may wait
(at a question, in the editor, for a paste, for a fetch): the candidate is saved, or removed, while
holding the lock shared (so a `configure exclusive` meanwhile waits for the write, or the write is
refused naming the holder), and a commit checks again after its question, before the broker is
asked. And every interactive session reads the shared candidate afresh at each command, and writes
it only if it is still as it read it (its bytes' hash, checked and written one session at a time):
a session idle since another saved it never writes back its old copy ("another session changed the
candidate since this one read it": the next command reads it as it is now) — with or without a
lock. A commit made while another session changed or took the candidate keeps that session's
candidate, and says so.

## The configuration tree

**Since S3c (docs/CONFIG-IS-THE-DEVICE.md) the tree is every NixOS setting of the system, one to one**
(`set virtualisation docker enable true`, `set services httpd virtualHosts example.com forceSSL
true`), generated from the system's own options (/etc/torq/nixos-schema.json) and kept in
`nixos/<top-level>.json`. A few settings are kept in files of TorqOS's own — for their own checks,
and for the user tier of a person's own folder — at their NixOS paths (ADR-025 #6: one place per
setting); the old names still work for setting them, and the tree shows the NixOS one:

| NixOS path | Old name | File |
|---|---|---|
| `networking hostName` | `system host-name` | `system/host.json` |
| `time timeZone` | `system time-zone` | `system/host.json` |
| `environment systemPackages` | `system packages` | `system/packages.json` |
| `users users <name> packages` | `users <name> packages` | `users/<name>/packages.json` (user tier) |
| `torq services enabled` | `services` (the old list) | `services.json` |

Software is set and shown at NixOS's own places, with TorqOS's unfree checks and messages; a person
added in the candidate (`set users users alice …`) can be given software in the same candidate.
Old names set and delete (and `delete users dev`, `delete system` take what they always did), but
the old *places* are not places to go: `edit users dev` is gone, as the tree now groups by NixOS. A
setting lives in exactly one place: those above are refused inside `nixos/` files, by torqcli and by
the evaluation (settings-fence.nix), naming the setting ("users users dev packages is kept in its own
file"). An error about one names it as it was typed (`networking hostName`, not the file's `system
host-name`). Text with a line break or another control character is refused in the tree: multi-line
text goes in a config file. Everything else in `nixos/` is system tier. What the tree may never
change is fenced (modules/settings-fence.nix), and **a person's password is never in the
configuration** (the fence's rule 3): `users users <name> password`, `hashedPassword`,
`initialPassword`, `initialHashedPassword`, `hashedPasswordFile` and `passwordFile` are refused, never
offered by `?`, saying why (everyone can read the configuration, and its history keeps every value)
and what to do (`request system password <user>`). Ledger reasons carry each change's section (`+
virtualisation docker enable true;`).

**Services are NixOS's own** (`set services openssh enable true`; modules/services.nix). TorqOS's
protections apply however a service is turned on, so there is no second, safer name for it. SSH: keys
only unless `services openssh settings PasswordAuthentication true`, and then repeated wrong guesses
are blocked (sshguard); 3 tries a connection (`MaxAuthTries`); only the owner may log in
(`AllowUsers`); root never logs in over SSH (an assertion: `PermitRootLogin` must be `no`). The
others are defaults a person can change in the tree. Podman's root API socket stays off. The old
catalog (`services.json`, a list such as `[ "ssh" ]`, and `torq ssh passwords`) keeps working on
machines that have it, mapped onto NixOS's settings, with a warning naming the settings to move to;
torqcli never adds to it, and `set services ssh` says what to set instead.

**Each setting's rules come from NixOS's own description of it** (ADR-025 #2), so `set` refuses what
commit would:

- Closed sets (true/false, an enum) are listed in `?` one by one, so Tab completes them; `set`
  refuses any other value.
- `set` checks what NixOS would: patterns (`strMatching`, the whole value), ranges (`intBetween`,
  `numberBetween` and every fixed-width integer), paths from `/`, non-empty text, account fields
  without `:`. `?` says the form ("a number from 1 to 100", "text matching …"); a refusal says it,
  with the setting's own help. A host name, user name or package name that is not one says what one
  is ("letters, digits and -, at most 63, …").
- Where NixOS takes any text but the value has a form (an interface address, a route, a gateway, a
  name server), TorqOS states the form as data in the schema (modules/nixos-schema.nix, `forms`),
  read by the same pattern check, and named in words: `'999.1.1.1' is not an IPv4 address`; `?`
  says `<an IPv4 address>`. A set named after something on the machine says so (`names`:
  `networking interfaces`), and `?` offers what the machine has (its interfaces).
- Read-only settings (`networking fqdnOrHostName` and 88 more) are left out of the tree: never
  offered, never settable.
- Setting names abbreviate as command words do (a unique beginning is the name, stored as the name;
  several are named). An entry's name is never guessed at.
- A section NixOS does not describe says what it holds ("Settings: enable, rootless"); where entries
  are named, `?` offers the configuration's own first.
- **Sections that take any key** (285 of them: RFC 42's `settings`, nginx `upstreams`, …) take a key
  as written, with that section's value type: `set services openssh settings ClientAliveInterval 60`.
  `?` offers `<key>` beside the section's own settings. Inside such a section names are exact (a
  beginning of one of its own is a key of its own: `Port` does not become `Ports`); the section itself
  still abbreviates.
- **Lists of entries with no names** (an interface's addresses) are set by number: `set networking
  interfaces eth0 ipv4 addresses 1 address 10.0.0.1`, `… addresses 1 prefixLength 24`; `… addresses 2
  address …` adds another. One past the end is refused, saying how many there are; `?` offers the
  numbers there are and the next. `show` and `| compare` say `addresses 1 { … }`; `delete … addresses
  1` removes it, the ones after it renumbering. In the files they are a JSON list, as NixOS reads it.
- **Package names** are checked and offered as they are set, in TorqOS's software lists and in any
  NixOS setting that takes a package: the schema carries this machine's nixpkgs names (only the
  names: nothing is evaluated). A name it does not have is refused with the closest ("'btopp' is not
  a package in this machine's nixpkgs (did you mean btop?): `show packages <word>` searches them");
  `?` and Tab offer the names, narrowed by what is typed. A name in a set of nixpkgs
  (`python3Packages.rich`) is one.

The rest of this section describes the files TorqOS keeps, as first designed.

Each container is a folder of the config repo; each leaf set is one small data file. The first
settings (more are added one schema entry at a time):

| Path in the tree | File in the config repo | Tier |
|---|---|---|
| `system packages <name>` (now `environment systemPackages`) | `system/packages.json` | system |
| `system host-name <name>` · `system time-zone <tz>` | `system/host.json` (done: `modules/host.nix`) | system |
| `users <name> packages <pkg>` (now `users users <name> packages`) | `users/<name>/packages.json` | user |

Validation (enforced by the NixOS modules' types, the authority — the shell's schema only gives early
errors and help): host names are RFC 1123 labels of at most 63 characters; time zones must exist in
tzdata (the ledger itself is always UTC); `<name>` under `users` must be a normal user of the machine.
Changing the host name never changes which configuration the broker builds: the config repo's flake
always exposes one fixed output, `nixosConfigurations.torq`.

**Held: environment variables.** `system environment` and `users <name> environment` need their own
design first (review N2/N3): variable values are effectively code (`PATH`, `LD_PRELOAD`, `BASH_ENV`,
`PROMPT_COMMAND`, `EDITOR`, `NIX_CONFIG`…), a user-tier value must never reach another user or root,
a system value must never reach the broker's environment, and people will paste secrets into them.
They come back with an allow-list of names, per-user delivery, escaped values and a secrets check.

Files outside the tree (`flake.nix`, any `.nix` not attached) are system tier and edited
as Nix, not through the shell; `show configuration` names them at the end (`## also set outside this
configuration, in Nix: …`, 5a), with the software the machine runs (`## software: …`), and
any change to one is shown as its raw diff in `show | compare` and in `commit`.

```
viper@torq-utm> show configuration
environment {
    systemPackages [ vim ];
}
networking {
    hostName torq-utm;
}
services {
    openssh {
        enable true;
    }
}
time {
    timeZone America/New_York;
}
users {
    users {
        viper {
            packages [ htop ripgrep ];
        }
    }
}
```

As built (cli/src/tree.rs, whole.rs): statements are sorted by name at each level, and `| compare`
shows changes under the innermost `[edit …]` section that holds them, a whole leaf-list as one
`-`/`+` pair, a new container as its leaves (`[edit virtualisation docker] + enable true;`, where a
network device would show `+ docker { … }` under `[edit virtualisation]`). A section's own settings
come together, before its sub-sections: `[edit networking]` once, not reopened after `[edit
networking firewall]`.

Config files, for what is not a value (docs/CONFIG-IS-THE-DEVICE.md, S3e):

```
[edit]
you# file edit services/httpd.nix          # $EDITOR; waits in the candidate
you# set services httpd config services/httpd.nix
you# show | compare
[edit services httpd]
+   config services/httpd.nix;
[edit config files]
--- /dev/null
+++ b/services/httpd.nix
…
```

`file list`, `file show <file>` (also in operational mode, of the active configuration), `file
delete <file>`. A place may hold several files: `set <place> config <file>` adds one (`set config
motd.nix` keeps `hardware-configuration.nix`), shown as `config [ a.nix b.nix ];`; `delete <place>
config <file>` unattaches one, `delete <place> config` all of them. `config-files.json` holds a list
per place (one name, the first form, still reads as a list of one).

## The commit model

- **Active configuration:** the config repo at its last commit. **Candidate:** your private copy
  (`~/.local/state/torq/candidate/`, with the commit it is *based on*), created by your first edit.
- **`commit`** sends the change from your base to your candidate as one proposal and applies it.
  If someone committed in between, the broker refuses (it checks the base): `rollback 0` and redo.
  Afterwards the candidate is gone.
- **`commit check`** evaluates the candidate as the broker will build it, without switching. Its
  result is kept with the candidate, keyed by a hash of the base and the exact changes (each file's
  path and content): a `commit` of exactly those changes uses it — saying so, `that check's result
  is used, not evaluated again` — instead of evaluating again. Any change to the candidate (one
  changed and changed back is the same), or a base that moved, is another key.
- **One evaluation per commit.** The check is one `nix eval` of the changed configuration's
  `config`: the toplevel's drvPath (so its errors and warnings appear, before anything is asked, a
  password typed or a change ledgered), SSH after the change (on, passwords, and what its keys are
  read from: the owner, their home and uid, the key files sshd reads, the keys declared), and
  whether it reads the tree (`configDir`). SSH *before* the change is needed only when SSH is on
  after it; it is a committed configuration with its lock, so it is kept by commit id in the
  person's own cache folder (`$XDG_CACHE_HOME/torq`, else `~/.cache/torq`: made 0700, and used
  only while it is theirs with no group or other bits at all; each entry read and written through
  the folder held open, never through a link; an entry that cannot be read is evaluated again).
  Entries, and a check's result in the candidate, are named by a hash of the Nix that read them
  (and the configuration name), so another torqcli's are never taken for this one's.
  After a commit the broker lands, its SSH is kept under the new HEAD (when HEAD is that commit: its parent the base,
  its files the change's), so the next commit evaluates once. A commit therefore evaluates once;
  twice when SSH is on after it and the commit it is based on was never evaluated here; not at all
  after a `commit check` of the same changes. The broker's own build of the ledgered commit is
  separate, and stays.
- **`| display detail`** after `commit` (and `commit check`, `commit confirmed <n>`, `commit at`,
  …) and after `request system update [all]` / `request system software rollback <n>`: each step
  named as it starts and how long it took (evaluating, SSH before it — or "from the cache" —
  proposing, the broker's build and switch), and Nix's and the broker's own output shown as it
  comes instead of the progress line — each line cleaned, as everything shown is, and still kept in
  the log. A pipe, as after `show`: these commands take `| display detail` alone (what they print
  is not data to filter). Without it, nothing changes.
- **Commit numbers count backwards:** 0 is the active configuration. They are the ledger entries
  that produced a new configuration (`init`, `applied`, `reverted`, `rolled_back`), newest first.
  Numbers shift
  with every commit, so `rollback <n>` says which commit it loaded (date, who, comment).
  `show system commit` reads the ledger, so it uses `sudo` like a commit does.
- **`rollback <n>`** loads commit <n>'s *configuration* into the candidate — every file except
  `flake.lock`, so it never downgrades the software (network devices keep the two apart, too). That
  includes `.nix` files (e.g. turning development mode back on): `show | compare` shows each one as
  a raw diff, flagged. `rollback 0` discards the candidate. `rollback ?` (and Tab) lists the
  configurations themselves, newest first, each with when, who and what.
- **`rollback <n> <place>`** undoes one part: that place goes back to how configuration <n> has it,
  the rest of the candidate stays as it is (`rollback 0 networking hostName`; any place, from where
  `edit` is, abbreviated). It says what it put back.
- **Change numbers** (the ledger's) are written `#85` everywhere: `show system ledger`, `show system
  commit` (beside each configuration's rollback number: `0 #85 …`), and `request system revert`
  takes them as written (`#85` or `85`). `show system ledger` names who made each change (the person
  who approved it, else who asked).
- **`request system software rollback <n>`** is the separate, explicit way to go back to commit
  <n>'s software (its `flake.lock`), marked as a downgrade.
- **`commit confirmed <minutes>`** (1–1440, default 10; with `comment "…"` in either order) is for
  changes that could cut you off:
  - the new system is *activated but not made the boot default* (`switch-to-configuration test`);
  - the window starts when the switch has finished;
  - a plain `commit` with an empty candidate confirms it: the broker makes the running system
    the boot default (no rebuild). With edits in the candidate, `commit` refuses — confirm first,
    then commit them. Only the person who made the confirmed commit confirms it (others are told
    whose it is; the broker refuses them too). As built (3c), torqcli says all this before
    evaluating, asking or proposing anything;
  - confirming is accepted until the broker has *started* the rollback (checked under its lock),
    not by comparing clocks, so a clock jump or a suspended machine can never leave a commit that
    can be neither confirmed nor rolled back;
  - if not confirmed in time, the broker restores the previous system **directly** (its store path,
    kept alive for the window — no evaluation, no download) and resets the config repo to the
    previous commit, as a ledgered change;
  - a reboot before confirming is itself the rollback: the boot default was never changed, and the
    broker records it at boot;
  - while it is pending, no other change can be made (except confirming, rolling back, or
    recovering that same commit if the broker crashed mid-way), so nobody confirms your risky change
    by committing their own; a second `commit confirmed` is refused until the first is settled.

```
viper@torq-utm# set services openssh enable true
viper@torq-utm# show | compare
[edit services openssh]
+   enable true;
viper@torq-utm# commit confirmed 5 comment "ssh, keys only"
viper@torq-utm# commit
```

- **`commit at <time>`** (with `comment "…"` and `and-quit`; not with `confirmed` or `check`):
  approved now, applied then. The time is `HH:MM` (today, or tomorrow if it has passed),
  `"YYYY-MM-DD HH:MM"` (local, as `date` reads it), or RFC 3339 with its offset; in the future and
  at most 30 days away (cli/src/at.rs; the broker checks both again). torqcli resolves it before
  evaluating or asking, then shows and asks as `commit` does (`… Commit at 2026-11-08 22:00:00?`),
  proposes the candidate with its base, and has the broker `schedule` it; the candidate is the
  proposal then, and is discarded. The broker (broker/src/at.rs):
  - is asked over the socket exactly as `apply` is (`schedule` is gated as `apply`: a person's own
    folder theirs, anything else an administrator's with their password), and refuses a tainted
    proposal, one without a base, one whose base is not HEAD, or one already applied;
  - arms a transient timer of its own (`commitAt` in broker.json: `systemd-run … --unit=torq-commit-at-<id>
    --on-calendar=<at> {broker} run-scheduled <id>`), then ledgers `apply_scheduled`: the proposal,
    the time, who, the timer's id, the base, and the hash of the diff approved;
  - at the time applies that proposal as that person: `applied` / `apply_failed` with `scheduled: <n>`.
    It is refused, `apply_refused` with `scheduled: <n>`, if the configuration has moved on from the
    base, a commit confirmed is waiting, the proposal was applied meanwhile, or the diff kept beside
    the ledger is not the one whose hash was approved; or if it is more than 15 minutes past its time
    (asleep then, a clock stepped since, a broker busy all along), or its approver may no longer (no
    account by that name, not a person's, or not an administrator, unless the change is wholly their
    own folder; read from /etc/passwd and /etc/group as the socket reads a caller). Under the broker
    lock, from the ledger: the
    timer's id names the schedule, never the proposal;
  - **one at a time**: a second is refused while one waits. Both would be prepared on the same
    configuration and the first changes it, so the second could only ever be refused at its time;
    refusing it now says so while the person is there (torqcli says so before asking anything). One
    more than 15 minutes past its time without a run is settled, refused on the record, by the next;
  - after a restart (the timer was transient) arms it again if its time is still to come, or refuses
    it if the time passed while the machine was down (`torq-commit-at-boot`, after `time-sync.target`:
    never applied late); each on its own, one that cannot be read or armed again refused on the record.
- **`clear system commit`** (operational) asks, then cancels it (`cancel-scheduled`: its approver's own,
  or an administrator's
  with their password): `apply_cancelled` in the ledger first, then the timer stopped. A run reads
  the ledger, so nothing outlives a cancel even if the timer could not be stopped.
  `show system commit` lists it above the configurations: `Scheduled: #57 at … by viper: …`.

The first `commit` shows the comparison and the keys that will be able to log in as viper (it
refuses if there are none), asks, and says when the change rolls back unless confirmed; the second
confirms it.

As built (3c/3d): `commit check` evaluates the candidate on the committed base (`git archive`, as
the broker builds it). What the configuration warns of (NixOS's `warnings`: a person's group
nothing declares, dev mode, …) is shown, `! warning: …`, by `commit check` and by `commit` (with
the comparison, before it asks; with the outcome when nobody is asked). A system change, or a change to another user's folder, shows the
comparison and needs a person's yes at a terminal; a change to your own folder does not (as
`torq pkg --yes` did; KNOWN-GAPS #4). Every value reaches the broker as `--name=value`. If the broker's
answer to a commit is lost, the ledger decides whether it was made (the candidate is discarded
only if it was). On a terminal the shell has history, Tab and `?`.

**Pasting** (how configurations travel): a paste arrives as typed — each line runs at its newline,
none is lost however the terminal sends it (with or without bracketed paste; rustyline keeps what it
read past one line). Every line of a paste runs and each error names its line, as on a network
device. A `commit` while lines have failed since you last looked at `show | compare` asks first
(`2 lines failed since you last looked at show | compare. Commit anyway? [y/N]`) — and a question
takes only an answer typed after it, so a paste can never answer it: a commit never takes part of a
paste unseen. No timing decides that. A question, or `load … terminal` asking for input, ends what
was pasted: the lines still waiting are shown as not run. While a command runs, keys typed are not
echoed over its output; they wait for the next line. Ctrl-C stops the running command (its child
process) and returns to the prompt. Scripts (`-c`, piped input) still stop at their first error.

## Operational commands

| Command | Does |
|---|---|
| `show configuration` | the active configuration |
| `show system commit` | the commit history, numbered for `rollback`, in local time (as the ledger); a commit scheduled with `commit at` first |
| `show system rollback <n> [compare <m>]` | commit <n>'s configuration, or what going from <n> to <m> changes (<n> is the base, as on a network device) |
| `show system alarms` | is this machine exactly as its history says? (and any pending confirmed commit) |
| `show system ledger [<count>]` | every change as the ledger has it (`#85`, who made it), refused and failed ones too |
| `show version` | what this machine runs: TorqOS (and where it comes from, from the lock), NixOS, the kernel, torqcli |
| `show system uptime` | how long the machine has run, its load, and its last change |
| `show interfaces [<interface>]` | each network interface's (or one's) state, addresses and hardware address; `?` offers the machine's |
| `show system storage` | each disk's size and use, and the system versions kept |
| `show packages <words>` | search this machine's nixpkgs: each package as it is typed (`htop`, `python3Packages.rich`), its version and description, closest first (the name itself, a set's package of that name, names beginning with the words, names holding them, then descriptions); `*` marks what the configuration installs |
| `set torq unfree packages [ <name> … ]` · `set torq unfree all true` | (configuration mode) allow unfree software (Postman, VS Code, …): by name, or all of it; refused otherwise, saying so (modules/unfree.nix) |
| `show services` | the services the configuration turns on (NixOS's `services <name> enable` and `virtualisation <name> enable`, and the old list), SSH with how it is protected |
| `request system password [<user>]` | change a person's password (kept in /var/lib/torq/secrets, never in the configuration) |
| `request system update [all] [\| display detail]` | load the newest TorqOS (and its nixpkgs) into the candidate; in operational mode at a terminal, with the software the candidate's only change, `Install it now? [y/N]` — yes commits it (one check; the password a system change asks), no leaves it for `configure; commit`. Other uncommitted changes in the candidate, configuration mode (`run request …`), a `-c` script or input that is not a terminal: loaded only, never committed by surprise, never a question a script could hang on. `torqcli request system update` as one command, its input and output a terminal (run from a bash script at one, too), does ask; `-c` never asks, nor does input that is not a terminal (`</dev/null`) |
| `request system software rollback <n> [\| display detail]` | load commit <n>'s software into the candidate (a downgrade); offered for install as an update is |
| `set system software torq source <source>` | (configuration mode) where TorqOS comes from: locks that source into the candidate |
| `save <file>` | (configuration mode) the whole candidate and its config files' content to a file, private |
| `load override\|merge\|set <file>\|terminal` | (configuration mode) a saved configuration into the candidate, all or nothing; `terminal`: what you paste next, ended by Ctrl-D (brace format, or `set` lines; `#` and `/* … */` comments are left out) |
| `request system repair` | return a machine changed outside TorqOS to its last commit (asks first) |
| `clear system commit` | cancel the commit scheduled with `commit at` (asks first; yours, or an administrator's with their password) |
| `request system revert <change>` | undo one change, `#85` or `85` (a new change that reverses it; asks unless it only touches your own settings) |
| `configure` / `config` | enter configuration mode |
| `configure exclusive` | enter it with the shared candidate held by this session alone, until it leaves: your other sessions read it, cannot change or commit it, and are told which session holds it |
| `help [<command> \| <words>]` | (both modes) the commands; one command; or a search of commands and settings |

(`save` / `load`: built, S5; `configure exclusive` and `commit at`: built, above.)

As built (3e, 4a, 4e): all of the above. The candidate holds the software too (4e): `request system
update [all]` runs `nix flake update` on the commit the candidate is based on and loads the new
`flake.lock`; `request system software rollback <n>` loads commit <n>'s; `show | compare` shows it as
`[edit system software]` (each input's date, a downgrade said so), `| display diff` as the raw
`flake.lock` diff, and `commit` (system tier: it asks) installs it. TorqOS itself is named by its
version where one is known (audit A3): `-   torq 0.2.1 (2026-11-05);` `+   torq 0.2.2 (2026-11-07);`.
The version before is the running one (`/etc/os-release`, only where it is TorqOS's, and only
against the configuration in effect); the one after is the release's, read from the very commit
fetched to check its signature (no second fetch): its number from `cli/Cargo.toml`, and the whole
tag from the signed commit's message (`TorqOS v0.3.0-rc1`, as release.yml writes it), where it names
that number — else the number alone, shown as `0.3.0, a release or a pre-release`. It is kept with
the candidate beside that commit (another commit in the lock: dates), and named when the release is
checked (`TorqOS release 0123456789ab (TorqOS 0.2.2): signed by …`). Two numbers that differ
decide, not the dates: an older one says `## TorqOS 0.1.9 is older than 0.2.1, which runs now: a
downgrade`. Only the numbers: the running version is its source's, which never says a pre-release
(a machine on 0.3.0-rc1 says 0.3.0), so a pre-release and its release, or the same number rebuilt,
go by date (`## older software than now: a downgrade`). A source that is not the releases, `request
system software rollback`, or a release without a version: dates, as before. `rollback <n>` loads a configuration and keeps the
software loaded into the candidate (and says so); `rollback 0` starts over without it. Loading onto a
candidate whose base is no longer the active configuration warns that `commit` will be refused.

`show system commit` reads the ledger (4f) — through the broker, as a commit does (`sudo`: the
ledger is root's, and stays unreadable to users) — its configurations, newest first, with who
approved each; without the broker there are no numbers, rather than git's; `rollback <n>`, `show system rollback <n>`
and `request system software rollback <n>` use the same numbers (`rollback 0`: what is active). `show packages` searches this machine's nixpkgs.
`show version`, `show system uptime`, `show interfaces`, `show system storage` and `show users` read
the running machine (/proc, `ip -j addr`, `df`, the system profiles, /etc/passwd and /etc/group,
`loginctl`; cli/src/facts.rs).

## How it is built

- **`torqcli` is unprivileged** (ADR-014): the shell, the candidate, `show`, `commit check` and
  `rollback <n>` live in it. A commit is one `propose --base` + `apply` over the broker's socket
  (ADR-040: no sudo; a system change asks the caller's password there, at the terminal).
- **The schema** is defined next to each setting's NixOS module (`torq.schema."<path>"`; the node
  format is documented in `modules/schema.nix`) and installed as `/etc/torq/schema.json`, so the
  shell reads it instantly for parsing, `?` and Tab. Closed sets carry their values (time zones from
  tzdata's index, the old list's service names, the machine's users); open ones a type (`package`,
  `host-name`). NixOS's settings are described in `/etc/torq/nixos-schema.json`
  (modules/nixos-schema.nix): each setting's type, values and range (read-only settings left out),
  the sections that take any key and their values' type, and this machine's nixpkgs package names. `tests/tree.nix` keeps it consistent: parents exist, files and keys fit the kind,
  `users <name> …` is user tier and nothing else is, and its user-tier files are exactly the
  allowed user leaves. Syntax is checked as you type; meaning (does the package exist, is it free)
  at `commit check`.
- **The schema is not the authority.** The NixOS modules' strict types are: every leaf file has a
  consumer that rejects bad data at evaluation, whoever wrote it (the shell, the agent, a raw diff).
  The installed schema belongs to the *running* system, so `commit check` evaluates the candidate
  against the candidate's own TorqOS version.
- **Reading history** for `rollback <n>`: the config repo is world-readable, read with git plumbing
  (`-c safe.directory=… -c core.hooksPath=/dev/null -c core.fsmonitor=false`). Secrets never go in
  the config repo (they get their own mechanism later).
- **The broker command is fixed**: `/run/wrappers/bin/sudo /run/current-system/sw/bin/torq-broker`.
  `TORQ_BROKER` and `TORQ_CONFIG_REPO` overrides exist only in test builds, so nothing in your
  environment can redirect the command your next `commit` runs as root.
- **Line editing**: a small line-editor crate for history, Tab and `?`.

## The broker's part

The broker stays small; its additions for this model:
- `propose --base <commit>`: `apply` refuses if the config repo's HEAD is not that commit.
- `apply --confirm <minutes>` (1–1440; done, 1f): records the anchor (previous commit and running
  system) and the kernel boot ID in the `applied` entry. In this order: builds the system (fixed
  `build` command, not the caller's), schedules the expiry, and only then activates it (`activate`,
  `switch-to-configuration test`) — so the new system never runs without a timer to undo it. The
  expiry is a transient wall-clock timer at the deadline (`--on-calendar`, `AccuracySec=1s`: it
  fires on resume if the machine slept through the deadline), retried every 15 s, at most 60 times
  in 20 minutes, while the broker is busy or the restore has not taken; it runs *this* — the old,
  known-good — broker by its store path. Instead of a separate GC root for the anchor, it requires
  the running system to be the boot default: the anchor is then the profile's current generation
  (rooted, and what a reboot starts). The new build is rooted by `state/confirm-system` until the
  change settles. If any step fails, or the broker is killed in any step, the restore goes
  straight to the anchor system, not a rebuild (`pending.json` records the anchor; the next
  operation — or the timer, once scheduled — reconciles). The apply's own activation is judged by
  its exit code, like a plain rebuild's (ADR-008): a unit that fails to start in the new system
  undoes the change at once.
- Every restore of an anchor is judged by the machine (running system and boot default are the
  anchor), not by exit codes (`switch-to-configuration` exits non-zero when a unit fails, after
  switching), and uses the anchor system's own commands and `PATH`
  (`<anchor>/etc/torq/broker.json`): the change being undone may have broken the new system's. The
  timer's broker reads the fixed file leniently: an unknown field (a newer system added it) is
  ignored, a missing one (renamed or removed) takes the default.
- `confirm`: requires the running system to be the one the commit activated, and the same boot
  (boot ID); sets the system profile to it and runs `switch-to-configuration boot` (no rebuild);
  ledger `confirmed`. Accepted until an expiry has started (under the lock), not by the clock. It
  reads what the applying broker wrote from the ledger only.
- `expire-confirm [<seq>] [--reboot]`: resets the repo to the anchor commit (keeping the discarded
  commit and any uncommitted change under `refs/torq/discarded/`), activates the anchor system
  directly unless it is already running, puts the boot default back if something moved it; ledger
  `rolled_back` with `unconfirmed: true`, `by_reboot` and what it found — once the restore has
  taken; until then it fails, stays pending and the change unsettled, so it is retried. After 40
  attempts that did not take it is ledgered `restored: false` and settled, so the machine is not
  frozen for ever; `status` reports it, and `repair` or a reboot fixes it. Nothing
  waiting is not an error (a confirmed commit's timer still fires). It is its own pending operation
  (`op: expire`): an interrupted one is finished by the next operation, not undone. The boot-time
  unit `torq-confirm-boot` (`Type=exec`: boot does not wait for it) runs `expire-confirm
  --reboot`, which acts only if the boot ID differs from the recorded one (a switch that first adds
  the unit starts it too).
- While a confirmed commit is pending, `apply`, `revert` and `repair` are refused (reconciling is
  not).
- The pending confirmation is derived from the ledger, never from a separate state file;
  `pending.json` has a version field so an older broker refuses a newer record clearly.
- `status` reports a pending confirmation (its deadline) instead of calling the boot default drift,
  and reports it as a problem once it is more than two minutes overdue.
- The broker clears its environment at start and takes its paths and rebuild commands from fixed
  configuration, not from the caller (closes part of KNOWN-GAPS #4). Done (1e): as root it reads
  only `/etc/torq/broker.json` (written by the module from `torq.broker.*`, including
  `rebuildCommand`) and refuses a flag or `TORQ_*` variable that differs, instead of ignoring it;
  without the file a root broker refuses to run. It clears its caller's environment (keeping only
  `TERM`; `LANG=C.UTF-8`), sets `PATH` and `HOME` from the file, tells git to read no global or
  system configuration, works from `/` (path arguments made absolute first) with umask 022. The
  fixed `PATH` starts with store paths of nix, git, coreutils, bash and systemd, so a
  change that drops one of them from the system cannot break its own `revert`; it takes the place
  of a sudo `secure_path`. The CLI calls sudo and the broker by absolute path. Without root
  (tests), flags apply.
- User-tier files are an explicit allow-list in `policy.rs` (`USER_LEAVES`, kept in step with the
  module's): `users/<name>/packages.json` (`cli.json`, the CLI style, until ADR-021) — a leaf joins
  only with its typed reader. Each must be a regular file (git mode 100644) of at most 64 KiB holding
  exactly its leaf's shape (`packages.json`: a list of at most 256 names). Evaluation rejects unknown files and symlinks under `users/` and
  names that are not normal users.
- The broker builds the commit, never the working tree: `{flake}` in its commands is
  `git+file://<repo>?rev=<commit>` (H4 of the second audit, 2026-10-31).
- The configuration the broker builds is always `nixosConfigurations.torq`, never derived from the
  host name (a VM test changes the host name, then commits again).
- The single-change undo is renamed `revert <seq>`, so "rollback" means one thing.

## The broker's JSON interface (ADR-014)

`torq-broker --json <command>` prints exactly one JSON value on stdout; `log --json` one per line.
torqcli itself has no `--json` (it is for people; programs use the broker's); `torqcli --verbose`
(`-v`, before the command) prints an error with every cause and where the whole log of the last
broker or Nix command is, instead of one line.
These shapes are an interface: change them deliberately, with tests on both sides.

| Command | stdout |
|---|---|
| `propose`, `apply`, `revert`, `show`, `repair` | the ledger entry |
| `apply --confirm <minutes>` | the `applied` entry, with `data.confirm_by`, `data.anchor_commit`, `data.anchor_system` |
| `confirm` | the `confirmed` entry |
| `expire-confirm` | the `rolled_back` entry (`data.unconfirmed: true`), or `{"expired": null}` |
| `log [--brief]` | one entry per line (`--brief`: without `data.diff`) |
| `reconcile` | `{"reconciled": <entry> \| null}` |
| `status` | `{ok, busy, problems[], …, confirm_pending: {seq, by, deadline} \| null}` |
| `verify` | `{"entries": n}` |
| `init` | `{"initialised": path, "ledger": path}` |
| any failure | `{"error": "...", "busy": bool}` |

A ledger entry is flat: `seq, ts, actor, approved_by, kind, tier, tainted, reason, data,
prev_hash, hash`. `kind` is one of `init, proposal, applied, apply_failed, reverted,
revert_failed, interrupted, repaired, confirmed, rolled_back`. `reverted` is one change undone by
`revert`; `rolled_back` is only an unconfirmed `commit confirmed` being undone.

Exit codes: **0** success; **1** refused or failed; **75** another operation is running; **2** bad
arguments. **JSON-escaped is not terminal-safe**: the CLI neutralises every string before printing.

## Acceptance criteria

- A network engineer drives it without reading this page.
- Someone who has never seen a router installs a package, undoes it and reads the history with only
  `?` and Tab.
- No command prints a commit hash, store path, ledger kind, `Caused by:` or a git hint by default.
- Every command has a snapshot test of its output (success, refusal, failure).
- VM tests: commit confirmed expires and restores; a reboot before confirming rolls back; a stale
  candidate is refused; `rollback <n>` never changes `flake.lock`.

## Related: the terminal itself

The login shell is TorqOS's (modules/shell.nix): zsh with oh-my-zsh, autosuggestions, syntax
highlighting, fzf history search and the TorqOS prompt (modules/shell/torq.zsh-theme: who and
where, the git branch, a warning when the machine is not as its history says, a marker when a
candidate of yours is waiting; plain Unicode, no patched font). All defaults: `set programs zsh
ohMyZsh theme <any oh-my-zsh theme>`, `set users defaultUserShell …`, a person's own ~/.zshrc, or
`torq shell enable false`. torqcli itself is in colour on a terminal (cli/src/paint.rs): `[edit …]`
sections bold cyan, `+`/`-` green/red, a configuration's places bold, settings cyan, braces and `;`
dim, `## …` comments dim, ✓/⚠ green/yellow, `!` warnings yellow, `?` lists with the word cyan and
its help dim, the prompt (user, host, mode) and `error:` in colour. It is added last, to text
already cleaned of anything that could fool a terminal, so a value can never bring its own codes;
and never with NO_COLOR, TERM=dumb, or when the output is not a terminal. There is no desktop
(ADR-019).

**Layout** (cli/src/ui.rs) — one set of rules for everything torqcli prints:

- **One width:** the window's, read for each command (a resized window is followed), at most 80
  columns and at least 20 (`ui::fit`).
- **A sentence is wrapped by the code that writes it** (`Ui::line`, `ok`, `warn`, `error`, with
  `ui::hang` underneath): its next lines start where its text started (after `! `, `✓ `, `error: `,
  a commit's date and author), never back at the left edge. Beside a lead wider than 4 with under
  20 columns left, the text goes on the lines after it, indented 4. Nothing guesses at print time
  which lines are sentences.
- **Every table is a `ui::Table`** (the `?` list, `help`, `show system commit`, the ledger,
  `show version`, `uptime`, `interfaces`, `storage`, `show packages`, status): data columns as wide
  as their widest cell and never broken (an address, a date, a change number); a heading row where
  the columns need naming; then the text column (a subject, a help), wrapped under where it starts,
  or — with under 20 columns beside — on the lines after, indented 4. A column may be capped (the
  `?` word column at 24): a wider cell stands on its own line and the rest of its row goes on below,
  in the columns. A column empty in every row takes no room.
- **The ledger is one line per entry**, its text cut to the window (`…`) and never wrapped, so a
  reason cannot draw what looks like another entry; who made each change is a column of its own,
  never cut (`⚠ tainted` included). `show system commit` shows each subject whole.
- **Taller than the window:** the pager (above); `| no-more` skips it.
- **Data is never wrapped or cut:** a configuration, `show | compare`, a diff, `| display set`;
  the terminal wraps a long line itself.
- **No window, nothing wrapped:** when a filter reads the output (`| grep`, `| wc -l` count lines)
  or it does not go to a terminal (a script), every sentence and row stays one line, as before.
- **Colour takes no room:** widths do not count colour codes. The test `every_window_size` runs
  real commands at 20, 40, 80 and 200 columns and without a window.
