# Commands

Every command can be abbreviated to a unique beginning, and `?` or `help` works anywhere.

## Operational mode

| Command | Does |
|---|---|
| `configure` / `config` | enter configuration mode |
| `show configuration` | the active configuration (<code>&#124; display set</code> prints it as `set` lines) |
| `show system alarms` | whether the machine is exactly as its history says, and any pending confirmed commit |
| `show system ledger [<count>]` | every recorded change, newest first, with its number (`#85`) |
| `show system commit` | the configurations, numbered for `rollback` |
| `show system rollback <n> [compare <m>]` | configuration `<n>`, or what going from `<n>` to `<m>` changes |
| `show version` | TorqOS, NixOS, the kernel and `torqcli` versions |
| `show system uptime` | uptime, load and the last change |
| `show interfaces [<interface>]` | network interfaces' state and addresses |
| `show system storage` | disks' size and use, and the system versions kept |
| `show packages <words>` | search this machine's packages (`*` marks installed ones) |
| `show services` | the services the configuration turns on, and how SSH is protected |
| `show users` | people with an account, who is logged in, and their groups |
| `request system password [<user>]` | change a password (kept outside the configuration) |
| `request system update [all]` | load the newest signed TorqOS release (`all`: and nixpkgs) into the candidate |
| `request system software rollback <n>` | load configuration `<n>`'s software into the candidate (a downgrade) |
| `request system revert <change>` | undo one recorded change (`#85` or `85`) |
| `request system repair` | return a machine changed outside TorqOS to its last commit (asks first) |
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
| `commit check` | build without switching |
| `commit confirmed [<minutes>]` | apply, and roll back by itself unless confirmed (1–1440, default 10) |
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

## From your shell

| Command | Does |
|---|---|
| `torqcli` | start the interactive shell |
| `torqcli <command>` | run one operational command |
| `torqcli -c "<commands>"` | run commands separated by `;`, in a private candidate |
| `torqcli --help` | how `torqcli` is run |
| `torqcli -v <command>` | on error, show every cause and where the full log is |
