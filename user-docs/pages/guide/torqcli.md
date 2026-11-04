# Using torqcli

`torqcli` is a shell with two modes, in the style of a network device. Anyone who has run a router
already knows how to drive it; anyone who hasn't can get by with `?` and ++tab++.

## Two modes

| | Prompt | What it is for |
|---|---|---|
| **Operational** | `alice@torq>` | Looking at the machine, history, updates, passwords |
| **Configuration** | `alice@torq#` | Changing the configuration in your candidate |

- `configure` (or `config`) enters configuration mode.
- `exit` at the top, `exit configuration-mode`, or ++ctrl+d++ leaves it (back to operational mode,
  not out of `torqcli`).
- `run <command>` runs an operational command from configuration mode, such as
  `run show system alarms`.

## Help is everywhere

- **`?`** lists what may come next, with a one-line description of each setting or command.
- **++tab++** completes the word you are typing.
- **`help`** lists the mode's commands; `help <command>` explains one; `help <words>` searches
  commands and settings (`help ssh`, `help hostname`).
- **History**: ++arrow-up++ recalls earlier lines.

If you use words from another CLI, `torqcli` tells you what TorqOS calls it. For example
`set system services ssh` points you at `services openssh`, and `| count` at `| wc -l`.

## Abbreviations

Any word can be shortened to a unique beginning: `sh conf | gr host` is
`show configuration | grep host`. If a beginning matches several words, `torqcli` names them; if it
is one letter off, it suggests the right word ("did you mean configuration?").

## Moving around the tree

In configuration mode you can move to a place in the tree, so you don't have to type the full path
each time:

| Command | Does |
|---|---|
| `edit networking` | go down to `networking` (the prompt shows `[edit networking]`) |
| `up` | go up one level (`up 2` for two) |
| `top` | go back to the top |
| `show` | show the candidate's configuration from where you are |

## Output: pipes and filters

After any `show`, add pipes:

- `| compare`: compare with the active configuration (`show | compare`).
- `| display set`: show a configuration as `set` commands, ready to paste elsewhere.
- `| display detail`: include the details normally hidden (commits, store paths).
- `| display diff`: show a comparison as the raw diff of the files.
- Filters, the real Linux tools with their usual options: `| grep`, `| wc`, `| head`, `| tail`,
  `| sort`, `| uniq`, `| cut`, `| tr`.

```text
alice@torq> show configuration | grep -i ssh
alice@torq> show system ledger | head -n 5
alice@torq> show configuration | display set
```

Filters run in a sandbox that can't reach your files or the network.

**The pager.** Output taller than the window is shown a screenful at a time:
++space++ for the next screenful, ++enter++ for one more line, ++n++ for the rest, ++q++ to stop.
Add `| no-more` to see everything at once.

## Pasting configurations

You can paste a block of `set` lines. Each line runs as it arrives, and each error names its line. If
any line failed, `commit` asks before going ahead, and a paste can never answer that question for
you.

To paste a whole configuration in brace format or as `set` lines, use `load merge terminal` (or
`load override terminal`), paste, and end with ++ctrl+d++.

## Saving and loading

| Command | Does |
|---|---|
| `save <file>` | write the whole candidate to a private file (relative names are in your home folder) |
| `load merge <file>` | add a saved configuration to the candidate |
| `load override <file>` | replace the candidate with a saved configuration |
| `load set <file>` | run a file of `set` lines |

## Scripts

Run one operational command, or a sequence of commands, from your normal shell:

```sh
torqcli show system alarms
torqcli "show configuration | grep host"
torqcli -c "configure; set users users alice packages htop; commit"
```

- A script's commands use a private candidate of their own, never your interactive one.
- A script stops at the first command that fails, and says how many after it did not run.
- The exit code says whether it worked.
- `torqcli --help` shows how `torqcli` can be run.

## Colours

`torqcli` uses colour only to mean something: additions green, removals red, warnings yellow. It
prints plain text when `NO_COLOR` is set, when `TERM=dumb`, or when output is not a terminal.
