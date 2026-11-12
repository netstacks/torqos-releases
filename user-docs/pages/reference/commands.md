# Commands

This page lists the commands of the `torqcli` prompt, and below them the `torq …` commands you run
from your login shell. `?` or `help` at the prompt is always the complete and current list. Every
command can be abbreviated to a unique beginning, and `?` or `help` works anywhere.

## Operational mode

| Command | Does |
|---|---|
| `configure` / `config` | enter configuration mode |
| `configure exclusive` | enter configuration mode with your candidate held by this session alone until you leave it: your other sessions can read it but not change or commit it, and are told which session holds it |
| `show configuration` | the active configuration (<code>&#124; display set</code> prints it as `set` lines) |
| `show system alarms` | whether the machine is exactly as its history says, and any pending confirmed commit |
| `show system ledger [<count>]` | every recorded change, newest first, with its number (`#85`) |
| `show system commit` | the configurations, numbered for `rollback`; a commit scheduled with `commit at` first |
| `show system rollback <n> [compare <m>]` | configuration `<n>`, or what going from `<n>` to `<m>` changes |
| `show version` | TorqOS, NixOS, the kernel and `torqcli` versions |
| `show system uptime` | uptime, load and the last change |
| `show interfaces [<interface>]` | network interfaces' state and addresses |
| `show system storage` | disks' size and use, and the system versions kept |
| `show system backups` | the declared backups: their snapshots and how each last ran |
| `show system snapshots` | the snapshots of your data that this machine still has, each from before a change |
| `show system proposals` | changes proposed and waiting for a person (the agent's) |
| `show system remote` | where the configuration and ledger are pushed, and how the last push went (see [The remote](../guide/remote.md)) |
| `show system desktop` | which of COSMIC's settings are saved in your configuration, and which are not yet (see [The desktop](../guide/desktop.md)) |
| `show system secrets` | the secrets: whose they are, whether each has a value, and what each may be used for (never a value) |
| `show packages <words>` | search this machine's packages (`*` marks installed ones) |
| `show services` | the services the configuration turns on, and how SSH is protected |
| `show users` | people with an account, who is logged in, and their groups |
| `request system password [<user>]` | change a password (kept outside the configuration) |
| `request system update [all]` | load the newest signed TorqOS release (`all`: and nixpkgs) into the candidate; <code>show &#124; compare</code> names its version beside the running one, and says when it is older (a downgrade). At a terminal, when the software is the candidate's only change, it asks `Install it now? [y/N]`: yes installs it (your password, as for any system change); no keeps it for `configure; commit`. With other uncommitted changes, from configuration mode, or in a `-c` script, it only loads. Run as one command (`torqcli request system update`, also from a shell script) at a terminal it asks; to never be asked, use `-c` or give it input that is not a terminal (`< /dev/null`) |
| `request system backup key <backup>` | give a backup's key, its passphrase (kept outside the configuration) |
| `request system restore <backup> [<snapshot>] <path>` | put a path back from a backup, what is there set aside |
| `request system software rollback <n>` | load configuration `<n>`'s software into the candidate (a downgrade); offered for install as an update is |
| `request system revert <change>` | undo one recorded change (`#85` or `85`) |
| `request system apply <n>` | apply a proposal of the agent's, after showing what it does (says why, if its session read untrusted text; one that could cut you off is applied as a commit confirmed) |
| `request system agent trust <minutes>` | for up to 60 minutes, the agent's untainted proposals in your own folder apply without asking (asks your password) |
| `request system agent trust revoke` | end that trust now |
| `request system snapshot take` | take a snapshot of your data now (asks your password) |
| `request system snapshot restore <n> <path>` | put a path back as it was before change `n` (asks your password) |
| `request system snapshot delete <n>` | delete one snapshot; recorded first, and cannot be undone (asks your password) |
| `request system pull` | load the configuration waiting on the remote into your candidate; nothing changes until you commit |
| `request system remote key` | show this machine's key for the remote, to add to the repository as a deploy key |
| `request system remote clear` | clear the remote's alarm after looking into it (recorded; raised again if still true) |
| `request system ledger acknowledge` | say that you have looked into a disagreement between the ledger and the repository (recorded) |
| `request system desktop save` | save your COSMIC settings into your configuration now |
| `request system desktop saving automatic` or `manual` | when your desktop is saved: as you change it, or only when you ask |
| `request system secret set <name>` | give a secret's value (typed, or `file <path>`): kept encrypted, outside the configuration, never in the ledger |
| `request system secret allow <name> https <host>` or `ssh <host>` or `agent` | let a secret be used for that host (a token in an HTTPS header, or an SSH key), or by the agent, which uses it without reading it |
| `request system secret disallow <name> …` | take a use back |
| `request system secret approve <name> each-use`, `first-use` or `never` | when a person is asked before the agent uses it (a session that read untrusted text is always asked). The approval step itself is not built yet: until it is, the agent may use only a secret that asks for no approval, from an untainted session |
| `request system secret show <name>` | print its value here (asks your password) |
| `request system secret delete <name>` | delete its value and its rule |
| `request system repair` | return a machine changed outside TorqOS to its last commit (asks first) |
| `clear system commit` | cancel the commit scheduled with `commit at` (asks first; yours, or an administrator's with their password) |
| <code>help [&lt;command&gt; &#124; &lt;words&gt;]</code> | the commands, one command, or a search of commands and settings |

## Configuration mode

| Command | Does |
|---|---|
| `set <setting> <value>` | set a value, or add one to a list |
| `delete <setting> [<value>]` | remove one value, or a setting and everything below it |
| `edit <place>` · `up [<n>]` · `top` | move around the tree |
| `show` | the candidate, from where you are |
| <code>show &#124; compare</code> | what a commit would change (<code>show &#124; compare rollback &lt;n&gt;</code>: against configuration `<n>`) |
| `commit` | apply the candidate (asks first) |
| `commit check` | evaluate the candidate as it would be built, to catch errors early; it does not build the system or switch to it. A `commit` of the same changes right after uses this check instead of repeating it |
| `commit confirmed [<minutes>]` | apply, and roll back by itself unless confirmed (1–1440, default 10) |
| `commit at <time>` | approve now, apply then: `HH:MM` (today, or tomorrow if it has passed), `"YYYY-MM-DD HH:MM"`, or RFC 3339; within 30 days, one at a time |
| `commit comment "<text>"` | commit with a note |
| `commit and-quit` | commit, then leave configuration mode |
| `rollback <n> [<place>]` | load configuration `<n>` (or one part of it) into the candidate |
| `rollback 0` | discard the candidate |
| <code>file edit&#124;show&#124;delete &lt;file&gt;</code> · `file list` | manage the configuration's Nix files |
| `set <place> config <file>` | attach a Nix file to a place |
| `save <file>` | save the candidate to a file |
| <code>load merge&#124;override&#124;set &lt;file&gt;</code> | load a saved configuration (`terminal` instead of a file: paste it) |
| `run <command>` | run an operational command |
| `exit` · `exit configuration-mode` · ++ctrl+d++ | leave configuration mode |

## Pipes

After any `show`:

| Pipe | Does |
|---|---|
| <code>&#124; compare</code> | compare with the active configuration |
| <code>&#124; display set</code> | as `set` commands |
| <code>&#124; display detail</code> | with commits, store paths and other details |
| <code>&#124; display diff</code> | a comparison as the raw diff of the files |
| <code>&#124; grep</code>, <code>&#124; wc</code>, <code>&#124; head</code>, <code>&#124; tail</code>, <code>&#124; sort</code>, <code>&#124; uniq</code>, <code>&#124; cut</code>, <code>&#124; tr</code> | the Linux tools, with their usual options |
| <code>&#124; no-more</code> | no pager |

After `commit` (any of its forms), `request system update` and `request system software rollback <n>`:

| Pipe | Does |
|---|---|
| <code>&#124; display detail</code> | each step as it happens and how long it took (checking, reading SSH, proposing, the build and switch), with Nix's and the broker's own output on screen instead of the progress line |

## From your shell

| Command | Does |
|---|---|
| `torqcli` | start the interactive shell |
| `torqcli <command>` | run one operational command |
| `torqcli -c "<commands>"` | run commands separated by `;`, in a private candidate |
| `torqcli --help` | how `torqcli` is run |
| `torqcli -v <command>` | on error, show every cause and where the full log is |

`torq` and `torqcli` are the same program. These `torq` commands are run from the login shell, not
typed at the `torqcli` prompt:

| Command | Does |
|---|---|
| `torq doctor "<question>"` | ask the agent a question about the machine; it reads and explains, and proposes nothing (experimental; see [The agent](../guide/agent.md)) |
| `torq agent propose "<ask>"` | as `doctor`, and the agent may propose one change |
| `torq agent run [--share <dir>] [--] [<command>]` | run a program (Claude Code, say) as the agent's account, in a sandbox, sharing one directory |
| `torq break-glass "<why>"` | in strict mode, the one way to a root shell: asks your password, records who and why, then opens it |
| `torq setup` | first-boot setup (the image's console, as root, until change #1) |
| `torq install` | the ISO installer (its console, as root) |
