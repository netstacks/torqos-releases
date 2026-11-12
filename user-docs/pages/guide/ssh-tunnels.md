# SSH tunnels

On TorqOS Desktop you can keep SSH tunnels to other machines up from the panel, for example a SOCKS proxy
that sends one browser profile's traffic and DNS through a server you trust. Tunnels belong to you, the
same way your Wi-Fi networks do. They are kept in your home folder, not in the configuration, and turning
one on or off asks no password and makes no commit.

## Add the applet

The applet is not in the panel by default. Open **COSMIC Settings → Desktop → Panel → Applets**
(or **Dock**), and add **SSH tunnels**.

## Before the first tunnel: check the host's key

A tunnel never accepts a host key it has not seen before. Connect once in a terminal and check the key
fingerprint it shows you:

```text
$ ssh you@192.168.50.45
```

A tunnel to a host whose key is not in your `~/.ssh/known_hosts` does not connect, and the applet tells
you so. Your key must also be one the far end accepts: a key in your keyring (`ssh-add -l`), or one of
your `~/.ssh` keys.

## Add a tunnel

Choose **Add tunnel…** in the applet, and give it:

- **Name**: lower-case letters, digits and `-`, such as `work`.
- **Destination**: `user@host`, or a `Host` from your `~/.ssh/config`. ssh still reads that file, so
  `User`, `Port`, `IdentityFile` and `ProxyJump` all work.
- **A SOCKS port** (such as `1080`), **or a forward** (`8080 intranet:80`).

Then switch it on. **Start at login** brings it up every time you log in.

## The file

The applet writes to `~/.config/torq/tunnels.toml`, and **Edit tunnels…** opens it. You can also edit it
by hand:

```toml
[work]
destination = "you@192.168.50.45"
forwards = ["socks 1080"]

[intranet]
destination = "bastion"          # a Host of ~/.ssh/config
port = 2222                      # optional
forwards = ["local 8080 intranet:80", "remote 2222 localhost:22"]
```

| Forward | What it does |
|---|---|
| `socks <port>` | A SOCKS5 proxy on this machine's `127.0.0.1:<port>`. Connections, and their DNS when the program asks the proxy to resolve names, come from the far end. |
| `local <port> <host>:<port>` | This machine's `127.0.0.1:<port>` reaches `<host>:<port>` as the far end sees it. |
| `remote <port> <host>:<port>` | The far end's `127.0.0.1:<port>` reaches `<host>:<port>` as this machine sees it. |

Nothing else goes in the file. ssh options such as `ProxyCommand` are refused, so the file cannot run a
command. Forwards listen on `127.0.0.1` only, so other machines on your network cannot use them. To check
the file, run `torq-tunnel check`; `torq-tunnel status` shows what each tunnel is doing. A mistake in one
tunnel stops only that one.

!!! note
    These guarantees cover `tunnels.toml`. Your own `~/.ssh/config` is still read, so what you put there
    still applies to a tunnel: a `ProxyCommand` of yours runs, and a `DynamicForward 0.0.0.0:…` of yours
    listens on the network.

## When a tunnel drops

A tunnel notices a dead link within about 45 seconds and reconnects by itself, waiting a little longer
after each failure (up to a minute). The applet's icon turns the warning colour while a tunnel that should
be up is not, and says why: the host is unreachable, its key is unknown, or it refused your key.
Tunnels run only while you are logged in.

## Firefox through a SOCKS tunnel

To send one Firefox profile through `socks 1080`, set **Settings → Network Settings → Manual proxy
configuration** in that profile: SOCKS host `127.0.0.1`, port `1080`, **SOCKS v5**, and tick **Proxy DNS
when using SOCKS v5**. While the tunnel is down, that profile's pages fail to load; they do not go out
directly.
