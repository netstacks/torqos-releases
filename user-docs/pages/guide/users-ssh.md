# Users, passwords and SSH

## Accounts

Accounts are part of the configuration. First-boot setup creates the owner's account; add more people
in configuration mode:

```text
alice@torq# set users users bob isNormalUser true
alice@torq# set users users bob extraGroups [ wheel ]
alice@torq# commit
alice@torq# run request system password bob
```

The `wheel` group lets bob use `sudo`; leave it out for an ordinary account. `show users` lists the
people with an account, who is logged in, and their groups.

!!! note
    By default only the owner may log in over SSH. To let other people in, change
    `services openssh settings AllowUsers`.

## Passwords

Passwords are **never** stored in the configuration: it is readable by everyone and keeps every value
forever. Each person's password lives in a file only the system can read.

```text
alice@torq> request system password          # your own
alice@torq> request system password bob      # someone else's
```

The ledger records *that* a password changed, when and by whom, never the password itself. Setting a
password in the tree (`users users <name> hashedPassword` and similar) is refused, and points you at
this command.

A person with no password can log in only with an SSH key, or not at all.

## Root

The `root` account is locked: nobody logs in as root. The owner borrows root's powers with `sudo`,
using their own password.

## SSH

SSH is NixOS's own `services openssh`, and TorqOS's protections apply however you turn it on:

- **Keys only**, unless you allow passwords.
- With passwords allowed, **addresses that keep guessing are blocked** at the firewall.
- **3 tries** per connection.
- **Only the owner** may log in (you can change this).
- **`root` never** logs in over SSH.

Turn it on, keys only:

```text
alice@torq# set users users alice openssh authorizedKeys keys "ssh-ed25519 AAAA… alice@laptop"
alice@torq# set services openssh enable true
alice@torq# commit confirmed 5
alice@torq# commit
```

Allow passwords too:

```text
alice@torq# set services openssh settings PasswordAuthentication true
```

A commit that changes who can log in over SSH shows exactly which keys will work, or that passwords
will be accepted. If SSH would be keys only with no keys at all, the commit is refused, because
nobody could log in.

`show services` says how SSH is protected, and `show system alarms` notes when SSH accepts
passwords.

!!! tip
    Use `commit confirmed` for any SSH change on a machine you reach over SSH. If the change locks
    you out, it undoes itself.
