# The agent

TorqOS's agent is an account of its own, `torq-agent`. It never changes your machine: it *proposes*
changes, and you apply them. What it may propose at all is the owner's list,
`torq agent mayPropose` (your own packages, by default).

## Ask it

```text
alice@torq> torq doctor "why is my wifi flaky?"        # reads and explains; proposes nothing
alice@torq> torq agent propose "install htop for me"   # may also propose one change
```

Both run as the agent, in a sandbox, for that one question. `torq agent propose` ends with a
proposal number, or nothing.

## What it read: taint

Logs, device names, the ledger and configuration values are text anyone could have written, and a
line in a log can carry instructions meant for the agent. So each time the agent reads such text, it
tells the broker first, and the broker marks the agent's session **tainted** — for good: nothing
the agent can do clears it. A proposal from a tainted session is marked so, and so is a proposal
from a program run as the agent that keeps no session with the broker (Claude Code under
`torq agent run`, for example): TorqOS cannot see what such a program read.

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
