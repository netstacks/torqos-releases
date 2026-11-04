# History and health

## The ledger

Every proposal and change is appended to the **ledger**: who asked, who approved, what changed, and
the system it produced. Each entry is chained to the one before it by a hash, so an edited entry is
detectable.

```text
alice@torq> show system ledger          # every change, newest first
alice@torq> show system ledger 10       # the last 10
alice@torq> show system commit          # configurations, numbered for rollback
```

Change numbers are written `#85`. Refused and failed changes are in the ledger too.

## Is the machine what its history says?

```text
alice@torq> show system alarms
```

TorqOS checks at boot, and every 15 minutes, that the machine is running exactly what the ledger
says. `show system alarms` reports anything that doesn't match, a confirmed commit waiting for
confirmation, and notes such as SSH accepting passwords.

## Changes made outside TorqOS

Only TorqOS writes the configuration. A stray build of `/etc/nixos` (where stock NixOS keeps its configuration) is refused. If
someone changes the machine another way (with `sudo`, say), the alarms say so. To put the machine
back to its last recorded state:

```text
alice@torq> request system repair
```

It asks before doing anything.

## The running machine

| Command | Shows |
|---|---|
| `show version` | TorqOS, NixOS, the kernel and `torqcli` versions |
| `show system uptime` | how long the machine has run, its load, and its last change |
| `show interfaces [<name>]` | each network interface's state and addresses |
| `show system storage` | each disk's size and use, and the system versions kept |
| `show services` | the services the configuration turns on |
| `show users` | people with an account, who is logged in, and their groups |
