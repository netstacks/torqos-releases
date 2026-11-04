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

## Checking a release yourself

See [Install → Check the download](../start/install.md#2-check-the-download). You need only OpenSSH
and coreutils.

## Reporting a vulnerability

Please report a suspected vulnerability privately to the maintainers rather than in a public issue.
