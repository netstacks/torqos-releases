# The agent

TorqOS's agent is an account of its own, `torq-agent`. It never changes your machine: it *proposes*
changes, and you apply them. What it may propose at all is the owner's list,
`torq agent mayPropose` (your own packages, by default).

!!! warning "Experimental"
    The agent's account, its proposals and its session trust exist and are tested, but no model
    ships with TorqOS, and no real model has ever been tried with it: the tests use a scripted
    stand-in. Whether a given model uses the tools well is unknown. Doctor mode is off by default.
    By default the agent may propose only the owner's own list of packages.

## Turn doctor mode on

Doctor mode is the part of the agent that answers questions. It is off until the owner turns it on,
as a recorded system change (`set torq agent doctor enable true`), and it needs a model to ask:

- a model file on this machine (`torq agent model url` and `hash`, which Nix fetches when the change is
  built, or `torq agent model path` for a file already on the disk), served by `llama-server`
  that runs from boot once on and holds the model in memory; or
- a remote endpoint (`torq agent doctor endpoint`). Then what the agent reads (logs, the
  configuration, which can contain a Wi-Fi password) leaves the machine, and it says so each time
  it starts.

The system journal is off limits to the agent unless `torq agent doctor journal true` is set. Turning
that on gives the whole agent account, and so every program run as it (Claude Code included), read
access to every service's log, so it is off by default whatever the model.

## Ask it

These are commands of your login shell, not of the `torqcli` prompt:

```text
alice@machine:~$ torq doctor "why is my wifi flaky?"        # reads and explains; proposes nothing
alice@machine:~$ torq agent propose "install htop for me"   # may also propose one change
```

Only an administrator can run them. Both run as the agent, in a sandbox, for that one question.
`torq agent propose` ends with a proposal number, or nothing.

## What it read: taint

Logs, device names, the ledger and configuration values are text anyone could have written, and a
line in a log can carry instructions meant for the agent. So each time the agent reads such text, it
tells the broker first, and the broker marks the agent's session **tainted** — for good: nothing
the agent can do clears it. A proposal from a tainted session is marked so, and so is a proposal
from a program run as the agent that keeps no session with the broker (Claude Code under
`torq agent run`, for example): TorqOS cannot see what such a program read.

This is a limit to know about. The broker holds only what it is told: TorqOS's own doctor reports
every read before the model sees it, but another program run as the agent can open a session and
never report, and its proposals then look untainted. What bounds that is the rest of the design:
trust covers only your own folder, lasts at most 60 minutes, needs your password, is recorded, and
never covers a system change.

## Apply a proposal

```text
alice@torq> show system proposals
alice@torq> request system apply 42
```

`request system apply` shows what the change does (as git writes it, not as the agent describes it)
and asks. For a tainted proposal it says what the session read, and you answer for that step alone.
A system change asks your password.

A change the agent proposes to the network or SSH, a service's unit or a file in `/etc`, the boot, the hardware, the console or the display could cut you
off. It is applied as a **commit confirmed**: it rolls itself back in 10 minutes unless you confirm
it (`configure`, then `commit`) — so if it cuts you off, you get your machine back by waiting.

## Trust it for a while

```text
alice@torq> request system agent trust 30        # asks your password
alice@torq> request system agent trust revoke
```

For the next 30 minutes (at most 60), the agent's proposals **in your own folder** — your packages —
apply as soon as it makes them, without asking you. Never a system change, never a proposal from a
tainted session, never someone else's folder. Each one is its own change in the ledger, marked with
your trust, and `request system revert <n>` undoes it. While it holds, `show system alarms` says so.
Only you can give trust for your folder, and you can end it at any time.
