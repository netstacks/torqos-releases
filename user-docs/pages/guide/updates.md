# Updates

TorqOS releases are published to
[netstacks/torqos-releases](https://github.com/netstacks/torqos-releases). Each release is signed
with the TorqOS release key, and the `stable` branch moves forward only to releases that passed the
tests.

## Update a machine

```text
alice@torq> request system update
alice@torq> configure
alice@torq# show | compare
alice@torq# commit
```

`request system update` fetches the newest release, **checks its signature** against the key your
machine trusts, and loads it into your candidate. An unsigned or wrongly signed release is refused
and nothing is loaded. You see the change in `show | compare` before committing it.

`request system update all` also updates the NixOS packages (nixpkgs) along with TorqOS.

Almost everything is downloaded pre-built and signed from NixOS's binary cache; the machine builds
only TorqOS's own small pieces.

!!! note "A new machine's first update"
    A freshly installed machine runs the TorqOS that came in its image. Its first
    `request system update` moves it onto the signed release channel; every update after that takes
    the newest signed release.

## Going back to earlier software

`rollback` restores configuration but keeps your current software. To go back to the software of an
earlier configuration, load it explicitly:

```text
alice@torq> request system software rollback 1
alice@torq> configure
alice@torq# show | compare
alice@torq# commit
```

`show | compare` marks it as a downgrade under `[edit system software]`.

## Which version am I running?

```text
alice@torq> show version
```

shows TorqOS (and where it comes from), NixOS, the kernel and `torqcli`.
