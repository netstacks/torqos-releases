# Your first session

`torqcli` is the TorqOS shell. It starts in **operational mode**, where you look at the machine.
`configure` enters **configuration mode**, where your changes go to a private *candidate* until you
commit them.

Run `torqcli` from your login shell to start it.

## Look around

```text
alice@torq> show system alarms
alice@torq> show version
alice@torq> show configuration
alice@torq> show system ledger
```

- `show system alarms` checks that the machine is exactly what its history says.
- `show configuration` is the whole machine's configuration.
- `show system ledger` lists every recorded change, newest first. Your setup is change `#1`.

Type `?` anywhere to see what may come next, and press ++tab++ to complete a word.

## Make a change

Turn on SSH and install `htop`:

```text
alice@torq> configure
alice@torq# set services openssh enable true
alice@torq# set environment systemPackages htop
alice@torq# show | compare
[edit environment]
+   systemPackages [ htop ];
[edit services openssh]
+   enable true;
```

Nothing has changed on the machine yet: these edits are in your candidate. `show | compare` shows
exactly what a commit would change.

## Check it, then commit it safely

```text
alice@torq# commit check
alice@torq# commit confirmed 5 comment "SSH on, keys only; htop"
alice@torq# commit
```

- `commit check` builds the new system without switching to it.
- `commit confirmed 5` shows the comparison and the keys that will be able to log in, asks, applies
  the change, and **rolls it back by itself in five minutes** unless you confirm it.
- The second `commit` confirms it.

If the change had cut you off, you would only have to wait: TorqOS puts the previous system back on
its own, and records that too.

## Undo it

Load the previous configuration into your candidate, look at what that would change, and commit:

```text
alice@torq# rollback 1
alice@torq# show | compare
alice@torq# commit comment "back to how it was"
```

Or undo one recorded change by its number, as `show system ledger` lists it (`#85` here):

```text
alice@torq> request system revert #85
```

## Leave

`exit` leaves configuration mode, and `exit` again leaves `torqcli`. Uncommitted changes stay in your
candidate for next time, and `configure` tells you about them.

Next: learn the shell in [Using torqcli](../guide/torqcli.md).
