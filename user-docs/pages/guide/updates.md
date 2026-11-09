# Updates

TorqOS releases are published to
[netstacks/torqos-releases](https://github.com/netstacks/torqos-releases). Each release is signed
with the TorqOS release key, and the `stable` branch moves forward only to releases that passed the
tests.

## Update a machine

```text
alice@torq> request system update
[edit system software]
-   torq 0.2.1 (2026-11-05);
+   torq 0.2.2 (2026-11-07);
Loaded into the candidate: `commit` (in configuration mode) installs it.
TorqOS release 3f2a9c1e0b7d: signed by the TorqOS release key.
Install it now? [y/N] y
Password for alice:
commit complete (change #57)
```

`request system update` fetches the newest release, **checks its signature** against the key your
machine trusts, and loads it into your candidate. An unsigned or wrongly signed release is refused
and nothing is loaded. It shows what changes, with TorqOS named by version — the one running, then
the release's — and asks whether to install it now. **Yes** checks the new configuration once and
installs it, asking your password as any system change does (and asking again only if there is
more to read first, such as a change to who can log in over SSH). **No** keeps it in your
candidate: `configure`, `show | compare`, `commit` installs it later.

It asks only when the update is all your candidate holds. If you have other changes not yet
committed, it loads the update next to them and says so: they are never committed by surprise, and
`configure`, `show | compare`, `commit` installs them together. From configuration mode
(`run request system update`), in a script (`torqcli -c "request system update; configure; show |
compare; commit"`) or with input that is not a terminal, it only loads, as before, and never stops
to ask. `torqcli request system update` run as one command is not a `-c` script: at a terminal
(even from inside a shell script) it asks; add `< /dev/null`, or use `-c`, where nobody will answer.

To watch what it does — each step, how long it took, and the build's own output — add
`| display detail`: `request system update | display detail`, or `commit | display detail`.

If the release is an older version than the one running, it says so plainly
(`## TorqOS 0.1.9 is older than 0.2.1, which runs now: a downgrade`), whatever its date. Where no
version can be read (a source other than the releases), each side is shown by its date. A
pre-release is named by its tag (`0.3.0-rc1`); since a machine reports only the number of what it
runs (`0.3.0` on a release candidate too), a pre-release and its release are compared by date.

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

It is shown as a downgrade under `[edit system software]`, and offered for install at once as an
update is (answer no, and `configure`, `show | compare`, `commit` does it as above).

## Which version am I running?

```text
alice@torq> show version
```

shows TorqOS (and where it comes from), NixOS, the kernel and `torqcli`.
