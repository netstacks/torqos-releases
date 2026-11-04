# Troubleshooting

## The VM shows a login prompt, not setup

First-boot setup runs on the first text console. A VM started with only a serial console shows login
prompts there, with no account to log in to. Give the VM a display and boot it again.

## The VM doesn't boot

- Make sure the VM uses **UEFI** firmware, not BIOS.
- Turn **Secure Boot off**. On Proxmox, don't pre-enroll keys on the EFI disk.
- On ESXi, make sure you copied the upload with `vmkfstools` and attached the copy, not the upload.

## `commit` is refused: someone committed in between

Your candidate was based on a configuration that is no longer the active one. Start from the current
one and redo your edits:

```text
alice@torq# rollback 0
```

## `commit` is refused: nobody could log in over SSH

SSH would accept keys only, and there are no keys. Add a key or allow passwords:

```text
alice@torq# set users users alice openssh authorizedKeys keys "ssh-ed25519 AAAA… alice@laptop"
```

## I'm locked out after a change

If you used `commit confirmed`, wait: the machine restores the previous system by itself when the
time runs out. Otherwise, from the console:

```text
alice@torq# rollback 1
alice@torq# commit
```

If the machine doesn't start at all, choose an earlier system version in the boot menu.

## A change is refused while another is pending

A confirmed commit is waiting for confirmation. Only the person who made it can confirm it (a plain
`commit` with nothing else in the candidate), or wait for it to roll back.

## `show system alarms` reports drift

Something changed the machine outside TorqOS. Look at what the alarms say, then:

```text
alice@torq> request system repair
```

## A package is refused as unfree

Allow it by name: `set torq unfree packages [ <name> ]`.

## Getting more detail on an error

Run the command with `torqcli -v` to see every cause and where the full log is.
