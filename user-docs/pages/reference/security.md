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
| History | A hash-chained ledger of every change, checked at boot and every 15 minutes |
| Footprint | No desktop, no graphical stack, no installer tools on an installed machine |

## How changes are controlled

- **One writer.** Only TorqOS's broker writes the machine's configuration. A stray build of
  `/etc/nixos` (where stock NixOS keeps its configuration) is refused.
- **A person approves.** A system change shows its comparison and needs a yes at a terminal.
- **Nothing hidden.** Any Nix file in a change is shown as its raw diff before you approve it.
- **Drift is visible.** Changes made around TorqOS (with `sudo`, for example) are reported by
  `show system alarms`, and `request system repair` puts the machine back.
- **The privileged code is small Rust**, with `unsafe` and dead code forbidden by the compiler.

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

## Checking a release yourself

See [Install → Check the download](../start/install.md#2-check-the-download). You need only OpenSSH
and coreutils.

## Reporting a vulnerability

Please report a suspected vulnerability privately to the maintainers rather than in a public issue.
