# Commit and roll back

## The candidate and the active configuration

- The **active configuration** is what the machine is running now.
- Your **candidate** is your private copy, created by your first edit. Nothing you `set` or `delete`
  changes the machine until you commit it.

Interactive sessions of the same user share one candidate. It survives leaving `torqcli`.

## Commit

| Command | Does |
|---|---|
| <code>show &#124; compare</code> | show what the commit would change |
| `commit check` | build the new system without switching to it |
| `commit` | show the change, ask, then apply it |
| `commit comment "<text>"` | commit with a note, kept in the history |
| `commit and-quit` | commit, then leave configuration mode |

`commit` prints the change it is making before it switches, and asks you to confirm at the terminal.
A change that doesn't build changes nothing.

If someone else committed since your candidate was started, your commit is refused: start again from
the new active configuration with `rollback 0` and redo your edits.

## Commit confirmed: changes that could cut you off

Changing SSH, the firewall or networking on a remote machine can lock you out. Use a confirmed
commit:

```text
alice@torq# set services openssh enable true
alice@torq# commit confirmed 5 comment "ssh, keys only"
alice@torq# commit
```

- `commit confirmed <minutes>` (1 to 1440, 10 if you leave it out) applies the change but **does not
  make it the boot default**.
- The timer starts once the switch has finished.
- A plain `commit`, with nothing else in your candidate, **confirms** it and makes it permanent.
- If you don't confirm in time, TorqOS **restores the previous system by itself** and records that in
  the history. Rebooting before confirming has the same effect.
- While a confirmed commit is waiting, no other change can be made, so nobody can confirm your risky
  change by committing their own. Only the person who made it can confirm it.

## Roll back

**Configuration numbers count backwards**: `0` is the active configuration, `1` the one before it,
and so on. `rollback ?` lists them, newest first, with when, who and what.

| Command | Does |
|---|---|
| `rollback <n>` | load configuration `<n>` into your candidate, to review and commit |
| `rollback <n> <place>` | put back just one part, such as `rollback 1 networking hostName` |
| `rollback 0` | discard your candidate |
| `show system commit` | list the configurations with their numbers |
| `show system rollback <n>` | show configuration `<n>` |
| <code>show &#124; compare rollback &lt;n&gt;</code> | compare your candidate with configuration `<n>` |

A rollback goes into your candidate like any other edit: look at `show | compare`, then `commit`.

`rollback` restores **configuration** only, never older **software**. To go back to an earlier
software version too, see [Updates](updates.md#going-back-to-earlier-software).

## Revert one change

Every change has a number in the ledger, written `#85`. To undo one change, whatever came after it:

```text
alice@torq> request system revert #85
```

This makes a new change that reverses the old one, and asks first unless it only touches your own
settings.

## The boot menu

Every committed configuration is also a bootable system version, listed in the boot menu. If a
machine ever fails to start, choose an earlier entry there.
