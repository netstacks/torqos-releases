# The remote

By default a machine's configuration and its ledger (the record of every change) live only on that
machine. A **remote** is a git repository, reached over SSH, where the machine also keeps a copy of
both. It is off by default. With it on, the configuration is a URL: a machine can be rebuilt from
the repository, and a ledger that someone with root rewrites on the machine disagrees with the copy
in the remote, which raises an alarm.

## Set it up

1. Make a repository for it, on GitHub or any git host you reach over SSH. Make it private, and
   turn on branch protection so that nobody can force-push to it: the copy of the ledger it holds
   can then not be taken back.
2. Give the machine the address, as a system change (it asks an administrator's password):

    ```text
    alice@torq> configure
    alice@torq# set torq remote url git@github.com:you/my-machine.git
    alice@torq# commit
    ```

    The address is SSH only: `ssh://host/path` or `user@host:path`.
3. Show the machine's own key for it, and add that to the repository as a deploy key with write
   access:

    ```text
    alice@torq> request system remote key
    ```

    The key is made the first time you ask.

From then on, after every change the broker pushes the configuration and the ledger there, never
forced.

## See how it is going

```text
alice@torq> show system remote
```

shows where it pushes, whether the machine has its key, how the last push went, and whether the
remote's copy of the ledger agrees with the one here. A remote that cannot be reached is recorded
and tried again later; it does not break anything.

## When the copies disagree

If the ledger here has been cut short or rewritten, or something was pushed on top of it, the
machine raises an alarm that `show system alarms` reports. Once you have looked into it, say so on
the record, and the alarm stops:

```text
alice@torq> request system remote clear
alice@torq> request system ledger acknowledge
```

`remote clear` clears the remote's alarm (it is raised again if it is still true); `ledger
acknowledge` says that you have looked into a disagreement between the ledger and the repository.
Each is itself recorded in the ledger.

## Take changes from the remote

Someone may change the configuration in the remote's repository (on its incoming branch). To bring
that change here:

```text
alice@torq> request system pull
alice@torq> configure
alice@torq# show | compare
alice@torq# commit
```

`request system pull` only loads what is waiting into your candidate. Nothing changes on the machine
until you review it and commit it.

## Start a machine from a repository

At [first-boot setup](../start/first-boot.md), if the machine is online, you may give the address of
a repository instead of answering the questions: the machine then becomes the machine that
repository describes, with its history, but with this machine's own hardware, software and
password. Setup shows what is in the repository (its users, the Nix files it attaches, its SSH
setting) and asks you to confirm, because that configuration will run as root on this machine.

## Limits

- The remote protects against a ledger rewritten on the machine only as far as its own repository is
  protected. Someone who controls the remote controls the copy.
- What goes to the remote is what is in the configuration repository, which every account on the
  machine can read. Passwords are kept outside it. See [Security defaults](../reference/security.md).
