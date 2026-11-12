# Security defaults

## Out of the box

| | Default |
|---|---|
| Network | **Nothing listens** until you ask for it: no SSH, and no link-local name answering (LLMNR, mDNS) |
| `root` | **Locked**, no password, never logs in over SSH |
| Accounts | The owner's account is created at first boot; there is no default password |
| Passwords | Kept outside the configuration, readable by the system only, so their hashes are never in its history |
| SSH (once on) | Keys only unless passwords are allowed; 3 tries per connection; the owner only; repeated wrong guesses blocked |
| Releases | Images and updates are signed; every update's signature is checked before it is loaded |
| History | A hash-chained ledger of every change, checked at boot and every 15 minutes. Against someone with root on the same machine it holds only if the configuration is also pushed to a remote, which is off by default |
| Footprint | The base has no desktop and no graphical stack, and no installer tools on an installed machine. A desktop (COSMIC) is an opt-in configuration: see [The desktop](../guide/desktop.md) |

## How changes are controlled

- **One writer.** Only TorqOS's broker writes the machine's configuration. The configuration tree refuses a list of settings that would change the broker itself, but plain Nix files attached to the tree, and other settings that run code as root, can still change it: what stands there is an administrator reading and approving the change. A stray build of
  `/etc/nixos` (where stock NixOS keeps its configuration) is refused.
- **A person approves.** A system change shows its comparison and needs a yes at a terminal (on a desktop, in a password dialog).
- **Nothing hidden.** Any Nix file in a change is shown as its raw diff before you approve it.
- **Drift is visible.** Changes made around TorqOS (with `sudo`, for example) are reported by
  `show system alarms`, and `request system repair` puts the machine back.
- **The broker is Rust** (about 9,000 lines, with `unsafe` and dead code forbidden by the compiler). Other things also run as root: a few shell scripts, and `torqcli` itself at setup and install. The broker's own systemd units are not sandboxed yet.

## Root and sudo

The owner can use `sudo`, as on any Linux, and a root shell can go around TorqOS. TorqOS does not try
to forbid that; it makes it visible instead, through the alarms and the ledger's hash chain.

**Strict mode** is the stronger choice, off by default: `set torq security strict enable true`. Then
`sudo` runs nothing you could get a shell from, and the one way to a root shell is
`torq break-glass "why"`. It asks your password every time, writes who and why into the ledger, sends
that to your remote if you have one, and only then opens the shell. Daily work needs no `sudo`: `torqcli`
does not use it.

Before turning it on, know what it costs. If the ledger cannot be written, on a full disk for example,
there is no shell and no fallback: the way back is the machine's console, starting an older
generation from before strict mode if one is left, and otherwise booting from other media. No account
may be in a group that amounts to root, such as `docker`; the commit is refused until it is out.

## Your data, before each change

A newly installed machine keeps a read-only snapshot of `/home` and `/var/lib` before each
system change, the newest ten. `show system snapshots` lists them, and `request system snapshot restore <n>
<path>` puts a path back as it was before change `n`. What is there now is set aside beside it, not
lost. It asks an administrator's password and goes in the ledger. Stop a service before restoring its
data, and start it again after.

Something you delete stays in the snapshots until they age out, ten system changes later. They hold your
files, not a backup of them: a failed disk takes them too. Machines installed on ext4 have none.

To change how they work: `set torq snapshots keep 20`, or `set torq snapshots enable false`, in configuration
mode. `request system snapshot take` takes one now, before work you may think better of, and
`request system snapshot delete <n>` removes one, for the room or for what is in it: that goes in the ledger
first, and there is no undoing it.

## Your data, somewhere else

Snapshots live on the same disk. A backup is NixOS's own restic, set in configuration mode like
any setting, to a disk, a NAS, SFTP or S3:

```
set services restic backups nas repository sftp:backup@nas:/torq
commit
```

It holds `/home` and `/var/lib` unless you set `paths`, and never `/var/lib/torq/secrets` (your
password hashes, and the backup's own key) unless `paths` names it. It runs daily, encrypted.

Its key, the repository's passphrase, is never in the configuration: `request system backup key nas`
asks for it here, twice, then your password, and keeps it where only the system can read it. The
ledger says it was given, never what it is. Keep it somewhere else too: without it, nothing in the
backup can be read. A remote target's own login, an SSH key or S3's keys, you place yourself.

`show system backups` lists each backup's snapshots and how it last ran. `request system restore nas
/home/you/docs` puts a path back from the newest snapshot, or from one you name:
`request system restore nas 4bd1d0c5 /home/you/docs`. As with a snapshot, it is written as the owner
of the folder, what is there now is set aside beside it, it asks an administrator's password, and it
goes in the ledger.

## Checking a release yourself

See [Install → Check the download](../start/install.md#2-check-the-download). You need only OpenSSH
and coreutils.

## Reporting a vulnerability

Please report a suspected vulnerability privately to the maintainers rather than in a public issue.
