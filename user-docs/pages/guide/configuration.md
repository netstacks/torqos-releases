# The configuration

On TorqOS, as on a router, one configuration defines the whole machine: what software it runs,
every system setting, every service, every user. You change the machine only by changing the
configuration, so the machine is always exactly what its configuration says.

## What the configuration holds, and what it doesn't

| | Changed with | Touched by commit or rollback? |
|---|---|---|
| Software, `/etc`, services, users, groups, boot | `torqcli` (`commit`) | Yes: it *is* the configuration |
| Home folders: your files, app settings | anything you like | Never |
| Service data: databases, containers, logs | the services themselves | Never |

A commit or a rollback never touches your data.

## The tree: NixOS's own settings

Every NixOS setting is a path in the tree, with spaces where NixOS has dots. So any NixOS guide
applies directly:

| In NixOS | In torqcli |
|---|---|
| `virtualisation.docker.enable = true;` | `set virtualisation docker enable true` |
| `services.httpd.enable = true;` | `set services httpd enable true` |
| `services.httpd.virtualHosts."example.com".forceSSL = true;` | `set services httpd virtualHosts example.com forceSSL true` |
| `environment.systemPackages = [ pkgs.htop ];` | `set environment systemPackages htop` |
| `networking.firewall.allowedTCPPorts = [ 80 443 ];` | `set networking firewall allowedTCPPorts [ 80 443 ]` |

The tree is generated from your machine's own NixOS version, so `?`, ++tab++, help and checks work
for every setting.

**Quoting.** Quotes are only needed for spaces and the shell's own characters: `"My Site"`, or a
value containing `|` or `;`.

## Changing settings

| Command | Does |
|---|---|
| `set <setting> <value>` | set a value, or add one to a list |
| `delete <setting> <value>` | remove one value from a list |
| `delete <setting>` | remove a setting and everything below it |
| `show` | show the candidate (from where you are in the tree) |
| <code>show &#124; compare</code> | what a commit would change |

`set` checks values as NixOS would: true/false, choices from a list, number ranges, IP addresses,
package names. A value that isn't valid is refused with the reason, before you get anywhere near a
commit. Deleting something that isn't set is a warning, not an error.

```text
alice@torq# set networking hostName web1
alice@torq# set time timeZone Europe/London
alice@torq# set networking interfaces eth0 ipv4 addresses 1 address 10.0.0.10
alice@torq# set networking interfaces eth0 ipv4 addresses 1 prefixLength 24
```

Lists of unnamed entries, like an interface's addresses, are set by number: `addresses 1`,
`addresses 2`, and so on.

## Software

Install software for the whole system, or for one person:

```text
alice@torq# set environment systemPackages htop
alice@torq# set users users alice packages ripgrep
```

Search this machine's packages from operational mode:

```text
alice@torq> show packages btop
```

A package name that doesn't exist is refused with the closest match ("did you mean btop?").

**Unfree software** (VS Code, Postman, …) is refused unless you allow it, by name or all of it:

```text
alice@torq# set torq unfree packages [ vscode ]
alice@torq# set torq unfree all true
```

## Config files, for what isn't a value

Some things aren't a value: Nix code, a hardware file, a custom module. These are plain Nix files
attached to the tree:

```text
alice@torq# file edit services/httpd.nix
alice@torq# set services httpd config services/httpd.nix
alice@torq# show | compare
```

- `file edit <file>` opens it in your editor (`$EDITOR`); the change waits in your candidate.
- `set <place> config <file>` attaches it to a place in the tree.
- `file list`, `file show <file>` and `file delete <file>` manage them.

Any change to a Nix file is shown as its raw diff in `show | compare` and at `commit`, so you always
see exactly what you approve.

## What the tree won't take

- **Passwords.** A person's password is never stored in the configuration, which everyone can read
  and which keeps every value forever. Use `request system password` instead
  (see [Users, passwords and SSH](users-ssh.md)).
- **Read-only settings**, and the settings only TorqOS itself may change. `?` never offers them.
