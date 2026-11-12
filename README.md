<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="branding/svg/lockup/torqos-lockup-for-dark-transparent.svg">
    <img alt="TorqOS" src="branding/svg/lockup/torqos-lockup-for-light-transparent.svg" width="460">
  </picture>
</p>

<h3 align="center"><code>set</code> &nbsp;|&nbsp; <code>commit</code> &nbsp;|&nbsp; <code>rollback</code></h3>

<p align="center">
  <strong>The configuration is the device.</strong><br>
  A thin, command-line operating system built on NixOS, where every change to the machine is<br>
  committed, recorded and reversible.
</p>

<p align="center">
  <a href="https://github.com/torqosorg/torqos-releases/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/torqosorg/torqos-releases?label=release&color=FF7A1A&labelColor=101216"></a>
  <a href="https://github.com/torqosorg/torqos/actions/workflows/test.yml"><img alt="Tests" src="https://img.shields.io/github/actions/workflow/status/torqosorg/torqos/test.yml?branch=main&label=tests&labelColor=101216"></a>
  <a href="LICENSE"><img alt="Licence: Apache 2.0" src="https://img.shields.io/badge/licence-Apache%202.0-5B6270?labelColor=101216"></a>
  <img alt="Platform: x86_64" src="https://img.shields.io/badge/platform-x86__64-8A93A3?labelColor=101216">
  <img alt="Built on NixOS 26.05" src="https://img.shields.io/badge/built%20on-NixOS%2026.05-7EE2A8?labelColor=101216">
</p>

<p align="center">
  <a href="https://torqos.org"><strong>torqos.org</strong></a> &nbsp;·&nbsp;
  <a href="https://docs.torqos.org">Documentation</a> &nbsp;·&nbsp;
  <a href="https://github.com/torqosorg/torqos-releases/releases/latest">Download</a> &nbsp;·&nbsp;
  <a href="docs/INSTALL.md">Install</a> &nbsp;·&nbsp;
  <a href="docs/ROADMAP.md">Roadmap</a> &nbsp;·&nbsp;
  <a href="branding/README.md">Brand</a>
</p>

<p align="center">
  <img alt="Logging in to a TorqOS machine over SSH: the TorqOS mark in letters, beside the machine's generation, its last change and whether it is as its ledger says" src="branding/screenshots/ssh-greeting.png" width="860">
</p>

---

## What TorqOS is

TorqOS is an operating system you configure the way you configure a router or a switch. The whole
machine (its services, users, packages, network, boot, and everything else NixOS can describe) is
**one configuration tree**. You change it from `torqcli`, a shell with operational and configuration
modes, a private candidate, `show | compare`, `commit`, `commit confirmed` and `rollback`.

Underneath, it is NixOS. Each commit becomes a git commit in the machine's own configuration
repository and a bootable NixOS generation, made by a small privileged **broker**, the only program
allowed to write that repository. Every step goes into a **tamper-evident ledger**, and any change
comes undone with one command.

It has **one base and two faces**:

<table>
  <tr>
    <td width="50%" valign="top">
      <h4>🖥️&nbsp; The server, as code</h4>
      A machine defined in code, in git, driven by <code>torqcli</code> and rebuilt from its
      repository anywhere. Every change reviewed with <code>show | compare</code>, applied with
      <code>commit</code>, undone with <code>rollback</code>. <b>This is what the released
      versions are.</b>
    </td>
    <td width="50%" valign="top">
      <h4>🤖&nbsp; The machine an AI can drive</h4>
      The same base with an optional desktop (COSMIC) on it, where an agent <i>proposes</i>, a
      person approves, and anything it did can be undone. The agent never gets root and never
      bypasses the broker. <b>Experimental:</b> the desktop is on the main branch and not yet in a
      release; the agent exists, but no model ships with it. See
      <a href="#project-status">Project status</a> and the <a href="docs/ROADMAP.md">roadmap</a>.
    </td>
  </tr>
</table>

> [!NOTE]
> **Status:** early releases. The latest is `v0.3.2` (2026-11-09). The configuration layer is
> complete, and its owner has used it daily since 2026-10-30; no one outside the project has
> tested it yet. A `0.4.0` release candidate (the desktop, and secrets for the agent) is in
> preparation and is not released. See [Project status](#project-status).

## Contents

- [Highlights](#highlights)
- [A first session](#a-first-session)
- [Install](#install)
- [Updates](#updates)
- [How it works](#how-it-works)
- [Security](#security)
- [Project status](#project-status)
- [Building from source](#building-from-source)
- [Repository layout](#repository-layout)
- [Documentation](#documentation)
- [Brand](#brand)
- [Contributing](#contributing)
- [Licence](#licence)

## Highlights

| | |
|---|---|
| 🌳 **One configuration tree** | Every NixOS setting, generated from the system's own options, with `?` help, Tab completion and abbreviations. `show configuration` is the whole machine; what is not a value (a hardware file, a module of your own) is a plain Nix file attached to the tree. |
| 🔁 **A network device's commit model** | Edit a private candidate, compare it with what runs, `commit check` to evaluate the candidate (it does not build it, and does not switch), `commit` to build and apply. `commit confirmed 5` rolls itself back unless you confirm it: the safe way to change SSH or networking on a remote machine. |
| ⏪ **Everything reversible** | `rollback <n>` loads any earlier configuration into the candidate; `request system revert #85` undoes one recorded change; the boot menu keeps every generation, and a commit confirmed that a power cut interrupts is rolled back at the next start. |
| 🔗 **A tamper-evident history** | Each proposal and change is a line in a hash-chained ledger: who asked, who approved, what changed, the system it produced. The machine checks at boot and every 15 minutes that it is what the ledger says. Against someone with root on the same machine this holds only if the configuration is also pushed to a remote, which is off by default. |
| ✍️ **One writer** | Only the broker writes the configuration repository. NixOS's tools for changing a machine are not on it, and a machine that drifts from its ledger says so. Root can still go around the broker: the machine then reports the drift afterwards; it does not prevent it. |
| 🔒 **Secure by default** | Nothing listens on the network until you ask. Root is locked. SSH, once on, is keys only unless passwords are allowed, the owner only, never root, with password guessing blocked. |
| 📦 **Signed releases** | Images come with signed checksums, and a machine checks the release key's signature on every update before loading it. Nothing unfree is in an image, and each release says where its source is. |
| 🪶 **Thin** | The base has no desktop and no graphical stack, and no installer tools on an installed machine: a new machine's system is held under a size budget CI enforces. A desktop (COSMIC) exists as an opt-in configuration, not as part of the base. |

## A first session

`torqcli` starts in operational mode. `configure` enters configuration mode, where changes go to
your candidate until you commit them.

```text
alice@torq> show system alarms
alice@torq> configure
alice@torq# set services openssh enable true
alice@torq# set environment systemPackages htop
alice@torq# show | compare
[edit environment]
+   systemPackages [ htop ];
[edit services openssh]
+   enable true;
alice@torq# commit check
alice@torq# commit confirmed 5 comment "SSH on, keys only; htop"
alice@torq# commit
```

`commit confirmed 5` shows the comparison and the keys that will be able to log in, asks, applies
the change, and rolls it back by itself in five minutes unless the second `commit` confirms it.

| Command | What it does |
|---|---|
| `show configuration` | The whole active configuration (`\| display set` prints it as `set` lines) |
| `show system ledger` | Every recorded change, newest first, with its number (`#85`) |
| `rollback 1` | Load the previous configuration into the candidate, to review and commit |
| `request system revert #85` | Undo one recorded change |
| `request system update` | Move the machine to the newest signed TorqOS release |
| `request system password alice` | Change a password (kept outside the configuration) |
| `show users` | People, their groups and who is logged in |
| `help`, `?` | Help for any command or setting, wherever you are |

Scripts use the same commands: `torqcli show system alarms`, or
`torqcli -c "configure; set users users alice packages htop; commit"`. Output filters are the real
tools, sandboxed: `show configuration | grep -i ssh | wc -l`.

The full design is in [docs/CLI-DESIGN.md](docs/CLI-DESIGN.md), the tree in
[docs/CONFIG-IS-THE-DEVICE.md](docs/CONFIG-IS-THE-DEVICE.md).

## Install

TorqOS is installed from an image: download it, check it, boot it, answer a few questions.

**1. Download** from the [latest release](https://github.com/torqosorg/torqos-releases/releases/latest):

| File | For |
|---|---|
| `torqos-x86_64-linux.qcow2` | QEMU/KVM, Proxmox VE, libvirt (x86_64) |
| `torqos-x86_64-linux.vmdk.gz` | VMware ESXi (x86_64); `gunzip` it first |
| `torqos-x86_64-linux.iso` | A real machine: the installer, written to a USB stick as it is (from `v0.2.0`) |

x86_64 only while TorqOS is in development. There are no aarch64 downloads for v0.3.x (v0.2.0 and
earlier have them); nothing builds or tests aarch64 now, and aarch64 images are planned to return
with the first stable release.

**2. Check it**, with the release's `SHA256SUMS`, `SHA256SUMS.sig` and `allowed_signers`, using only
OpenSSH and coreutils:

```sh
ssh-keygen -Y verify -f allowed_signers -I torqos-release -n file -s SHA256SUMS.sig < SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
```

The first command reports `Good "file" signature for torqos-release with ED25519 key SHA256:…`.
Compare that fingerprint with the one in the release notes and in this repository
([release/allowed_signers](release/allowed_signers)), so the key is not just the download's word.

**3. Boot it** as a UEFI virtual machine with a display, or boot the installer on a real machine:
it lists the machine's disks, never its own, asks you to name the one to erase, writes TorqOS to it,
reads it back, and leaves the machine's detected hardware for setup. The image has no user account,
root is locked, SSH is off and nothing listens on the network: the console shows first-boot setup.

**4. Answer setup's questions**, asked once, like a network device's initial dialog. Setup first
looks at the network (a cable is used as it is; for Wi-Fi it lists the networks it sees and joins
the one you choose; you may skip it). If it is online it asks whether this is a new machine or
should start from a configuration repository of yours (to become the machine that repository
describes); offline, it is a new machine. For a new machine it asks the machine's name and time
zone, your user name and password, and whether to allow logging in from other machines (not at
all, by password, by SSH key, or both; the default is not at all). Setup writes the configuration,
the broker builds and switches to it, and records it as change #1. A setup cut short before that
starts again from the beginning.

Step by step, for each hypervisor and for real hardware: [docs/INSTALL.md](docs/INSTALL.md).

## Updates

Releases are published to [torqosorg/torqos-releases](https://github.com/torqosorg/torqos-releases):
each a git commit signed with the TorqOS release key, with `stable` moved forward only to releases
that passed the tests. On a machine:

```text
alice@torq> request system update
alice@torq> configure
alice@torq# show | compare
alice@torq# commit
```

`request system update` loads the newest software into your candidate, after verifying the
release's signature against the key the machine trusts (`/etc/torq/release-signers`): an unsigned or
wrongly signed release is refused and nothing is loaded. You see the change before committing it, and
`request system software rollback <n>` goes back to an earlier version. Almost everything comes
pre-built and signed from NixOS's binary cache; the machine builds only TorqOS's own small pieces.

## How it works

```text
┌──────────────────────────────────────────────────────────────────┐
│ torqcli      the shell: the tree, the candidate, compare, commit │
│              (people; an agent's proposals, experimental)        │
├──────────────────────────────────────────────────────────────────┤
│ torq-broker  the only writer of the configuration repository:    │
│              checks, commits, builds, switches, confirms,        │
│              rolls back; appends every step to the ledger        │
├──────────────────────────────────────────────────────────────────┤
│ NixOS        the configuration repository (/var/lib/torq/config) │
│              → a NixOS generation, bootable, listed at boot      │
└──────────────────────────────────────────────────────────────────┘
```

A commit travels like this:

1. `torqcli` sends the difference between the configuration your candidate started from and your
   candidate to the broker, as one proposal, with your comment.
2. The broker checks it: the base is still current, nothing touches what only the broker may
   change, and a system change has a person's approval at a terminal.
3. It commits the change to the configuration repository, builds the new system and switches to it.
   A change that does not build changes nothing.
4. It appends the outcome to the ledger: the proposal, who approved it, the commit and the system.
5. A confirmed commit waits for confirmation; unconfirmed, the broker restores the previous system
   and configuration by itself, and records that too.

Two repositories, on purpose: **this one** is the distribution (NixOS modules, the broker and the
shell), built the same way by everyone; **each machine's configuration repository** is a small flake
that imports a TorqOS release and holds that machine's settings, written only by its broker. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Security

- **Nothing listens unless asked.** No SSH, no link-local name answering (LLMNR, mDNS): checked on
  the image by a VM test.
- **No default password.** Root is locked and never logs in over SSH; the owner's account is made at
  first boot. Passwords live outside the configuration repository, readable by the system only, so
  their hashes are not in its history.
- **SSH, safely, however it is turned on:** keys only unless passwords are allowed, three attempts per
  connection, the owner only (changeable), never root; with passwords, addresses that keep guessing
  are blocked at the firewall. A commit that changes who can log in shows it, and is refused if it
  would leave nobody able to.
- **The configuration is fenced from the broker, with limits.** A list of settings that would change
  what the broker runs, its files in `/etc`, its units, or TorqOS itself are refused when the system
  is evaluated, however they are spelled. The fence is a list: plain Nix files attached to the tree,
  and other settings that run code as root, can still change the broker. What stands there is an
  administrator reading and approving the change (KNOWN-GAPS 3 and 6m).
- **Signed releases**, for images and for updates, checked with OpenSSH alone.
- **An auditable history.** The ledger is hash-chained; `torq-broker verify` detects an entry edited, added or removed inside the chain (root can still rewrite it whole, or cut its newest entries). Pushing the configuration and the ledger to a remote repository catches that (built: `set torq remote url`), but it is off by default, so on a default machine the ledger does not hold against root.
- **The broker is Rust, and not all of what runs as root is.** The broker is about 9,000 lines under
  `broker/src` (tests excluded, comments and blank lines counted), with `unsafe` and dead code
  forbidden by the compiler. Also run as root: a few shell scripts in `modules/` (the agent's
  launcher, break-glass, the data snapshots, the lid handler on a desktop) and `torqcli` itself at
  setup and install. The broker's own systemd units are not sandboxed yet (KNOWN-GAPS N5 and 6zf).

The threat model, what is and is not protected today, and the plan for owner-rooted boot security are
in [docs/SECURITY-MODEL.md](docs/SECURITY-MODEL.md); open issues are tracked openly in
[docs/KNOWN-GAPS.md](docs/KNOWN-GAPS.md). Please report a suspected vulnerability privately to the
maintainers rather than in a public issue.

## Project status

| Milestone | | State |
|---|:---:|---|
| **M1** — the trust loop: propose, apply, rebuild, ledger, one-command rollback | ✅ | Done |
| **M1.5** — `torqcli`: the tree, the candidate, the commit model | ✅ | Done. A usability study with new users is still to do: no one outside the project has tested it |
| **M2** — install: images, first-boot setup, signed releases, the ISO installer, a thin base | ✅ | Done; released as `v0.2.0` |
| **M3** — trust: identity from the kernel, the agent's account, the config repo's remote (off by default), strict mode, data snapshots | ✅ | Done; in `v0.3.x` |
| **M3.5** — the install on real hardware | 🟧 | Partly: an offline first build, network-first setup, and TorqOS installed on the owner's laptop. Not built: the console UI, an answers file for unattended installs, recipes (shared configuration repositories), the installer's other ways (beside another system, a partition) |
| **M4** — the desktop as a configuration, applications, data | 🟧 | Largely built, on the main branch and not released: COSMIC, TorqOS Settings, a panel applet, SSH tunnels, approval as a dialog. It has run on one physical machine, the owner's laptop. Not built: wrapping COSMIC's own settings pages, applications by rung, syncing data |
| **M5** — the agent: doctor mode, proposals, session trust | 🟧 | The non-GUI half is built, in `v0.3.x`, and is experimental: the doctor is off by default, no model ships with it, and no real model has been tried (the tests use a scripted one). The rest is parked until after `0.4.0` |
| Later — owner-rooted boot security: the owner's Secure Boot keys, TPM2, disk encryption | ⬜ | Designed (ADR-033, [docs/SECURITY-MODEL.md](docs/SECURITY-MODEL.md)); not built |

Released: `v0.1.0` to `v0.3.2`. Not released: `0.4.0-rc1`, in preparation, which is the desktop and
secrets (ADR-052, which lets the agent use a token or key without being able to read it).
The roadmap is [docs/ROADMAP.md](docs/ROADMAP.md); design decisions and their reasons are recorded as
ADRs in [docs/DECISIONS.md](docs/DECISIONS.md).

## Building from source

You need Nix with flakes, on x86_64 Linux. The flake still evaluates for aarch64, but nothing builds
or tests aarch64 now. The VM tests need KVM to run quickly; they fall back to emulation without it.

```sh
nix flake check                                  # everything below
nix build .#image                                # the qcow2 image (.#image-vmdk for ESXi)
nix build .#iso                                  # the ISO installer
nix build .#checks.x86_64-linux.trust-loop       # one VM test
nix develop -c sh -c 'cd cli && cargo test'      # torqcli's unit tests
nix develop -c sh -c 'cd broker && cargo test'   # the broker's unit tests
nix develop -c sh -c 'cd wire && cargo test'     # torq-wire, the words the three share
```

| Check | What it proves |
|---|---|
| `tree` | The configuration tree is read as documented, and what it may never set is refused (evaluation only) |
| `trust-loop` | Commits, confirmed commits, rollbacks, the ledger and the shell, on a real NixOS |
| `socket` | The broker's door: who is asking is the kernel's answer; a system change takes an administrator's password; five wrong lock it, at sudo and su too |
| `agent` | The agent's account proposes only what its list allows, every refusal ledgered; a person sees what a proposal does and applies it |
| `agent-run` | A program (Claude Code) run as the agent: one shared directory, nothing else of any home, nothing that outlives it |
| `remote` | Every change pushed to the remote; a ledger cut short or rewritten here — or pushed on top by root — is an alarm |
| `adopt` | A new machine chooses a repository at setup and becomes that machine: its history goes on, with its own hardware, software and password |
| `snapshots` | A snapshot of the data before each change, read only, the ledger never in one; a restore runs as the folder's owner, so a planted link leads nowhere they could not go |
| `strict` | An administrator's sudo runs break-glass and the agent's launcher and nothing else; a root shell leaves its ledger entry, and pushes it, before it opens |
| `accounts`, `ssh` | Who can log in, and how; passwords never in the configuration |
| `image` | The release image from first boot through setup to its first signed update |
| `installer` | The ISO from a USB stick to a chosen disk, and that disk's first boot |
| `boot` | The boot menu on a real EFI partition: a commit's entry, an older one chosen, a power cut mid-commit |
| `free`, `size` | Nothing unfree in an image or the ISO; a new machine within its size budget |
| `first-build-offline` | A first boot's configuration builds from what the installer carries, with nothing downloaded |
| `commit-at` | `commit at`: a change approved now lands at its time, as the person who approved it; a cancelled one does not |
| `doctor` | Doctor mode against a scripted model: it reads, a hostile log line taints the session, a tool that writes is refused, nothing is proposed |
| `agent-proposals` | The broker holds each session's taint; a tainted proposal says why; a change that could cut the machine off applies only as a commit confirmed; session trust is bounded |
| `release-path` | What a changed `flake.lock` builds is what it names; TorqOS's own releases must be signed by the release key |
| `backup` | The declared backup: its key kept outside the configuration, a restore as the folder's owner, every restore in the ledger |
| `desktop` | The desktop boots to COSMIC's greeter and logs in; the approval dialog works (software rendering; smoothness is judged on real hardware) |
| `tunnels` | A person's SSH tunnels come up, listen on 127.0.0.1 only, refuse an unknown host key, and turn nothing in their file into an ssh option |
| `secrets` | A secret is used by a program and never read by it (ADR-052) |
| `cosmic-modules`, `cosmic-prebuilt`, `wire` | The COSMIC modules are nixpkgs' unchanged; COSMIC is built from the cache unless the owner turns custom compiling on; the wire crate's own tests |
| `broker` | The broker's own tests, in the build sandbox |

CI's `test` workflow runs `nix flake check` on pull requests, when started by hand, and inside each
release; it does not run on every push to `main`. The `image` workflow builds the x86_64 images, and
the `release` workflow tests, builds, signs and publishes a release on each `v*` tag. Developing
TorqOS: [docs/DEV-SETUP.md](docs/DEV-SETUP.md).

## Repository layout

```text
flake.nix          the entry point: packages, images, the ISO, VM tests, the machine template
modules/           the distribution, as NixOS modules (torq.* options)
cli/               torqcli, the shell, and the ISO's installer (Rust)
broker/            torq-broker, the privileged writer, ledger and rollback (Rust)
agent/             torq-doctor, the agent's program: doctor mode and proposals (Rust; experimental)
wire/              torq-wire, the words of the broker's socket and the ledger, shared by the three (Rust)
settings/          TorqOS Settings, the panel applet and the SSH-tunnels applet for the desktop (Rust)
scripts/           the release helpers (bump-version) and the COSMIC update helper
templates/config/  the configuration repository each machine starts from
hosts/             the image, the ISO installer, a development VM
tests/             NixOS VM tests and the configuration tree's checks
release/           the release key's public half (allowed_signers)
branding/          the mark, logos, icons, terminal logos, palette and screenshots
user-docs/         the documentation site, docs.torqos.org
docs/              design, decisions, install, security, roadmap
.github/workflows/ test, image, release and the weekly nixpkgs update
```

## Documentation

The user guide is at **[docs.torqos.org](https://docs.torqos.org)**. The design documents are here:

| Document | What it covers |
|---|---|
| [VISION](docs/VISION.md) | What TorqOS is for, and what it deliberately is not |
| [ARCHITECTURE](docs/ARCHITECTURE.md) | The layers, the change flow, the broker and the ledger |
| [CLI-DESIGN](docs/CLI-DESIGN.md) | `torqcli`: modes, commands, the commit model |
| [CONFIG-IS-THE-DEVICE](docs/CONFIG-IS-THE-DEVICE.md) | The configuration tree and its files |
| [INSTALL](docs/INSTALL.md) | Images, the ISO, checking a download, hypervisors, first-boot setup, updates |
| [SECURITY-MODEL](docs/SECURITY-MODEL.md) | Threat model, defaults, accounts and passwords, releases |
| [DECISIONS](docs/DECISIONS.md) | Architecture decision records |
| [ROADMAP](docs/ROADMAP.md) | What is done and what comes next |
| [KNOWN-GAPS](docs/KNOWN-GAPS.md) | Open issues and limits, stated plainly |
| [AI-LAYER](docs/AI-LAYER.md) | The agent, and how the design keeps it safe |
| [DEV-SETUP](docs/DEV-SETUP.md) | Developing TorqOS |

## Brand

<p>
  <img alt="The TorqOS mark" src="branding/svg/mark/torqos-mark-for-light.svg" width="72">
  &nbsp;&nbsp;
  <img alt="The TorqOS app icon" src="branding/svg/app-icon/torqos-icon.svg" width="72">
  &nbsp;&nbsp;
  <img alt="The TorqOS app icon, Torque" src="branding/svg/app-icon/torqos-icon-torque.svg" width="72">
</p>

The mark is a counter-clockwise ring (torque, and turning back on itself, rollback) around a `>_`
prompt, which is how TorqOS is configured: **set | commit | rollback**. The wordmark sets **Torq** in
the ground's text colour and **OS** in Torque orange.

| | Name | Hex | Use |
|---|---|---|---|
| ![](https://img.shields.io/badge/-%20%20%20%20-101216) | Graphite | `#101216` | Dark ground; the app icon's tile |
| ![](https://img.shields.io/badge/-%20%20%20%20-FF7A1A) | Torque | `#FF7A1A` | The accent on dark grounds: the ring, "OS" |
| ![](https://img.shields.io/badge/-%20%20%20%20-E2620B) | Torque on light | `#E2620B` | The accent on light grounds |
| ![](https://img.shields.io/badge/-%20%20%20%20-ECEDEF) | Foreground | `#ECEDEF` | Text and the prompt on dark grounds |
| ![](https://img.shields.io/badge/-%20%20%20%20-8A93A3) | Steel | `#8A93A3` | Secondary text |
| ![](https://img.shields.io/badge/-%20%20%20%20-5B6270) | Slate | `#5B6270` | Muted interface elements |
| ![](https://img.shields.io/badge/-%20%20%20%20-7EE2A8) | Signal green | `#7EE2A8` | "OK" states: verified, secure |

Type: **Space Grotesk** Bold for the wordmark and headings, **JetBrains Mono** for the command line.
On a machine, the same identity is the console's greeting before login, the banner over setup and the
installer, the shell's prompt and highlighting, and the SSH greeting above. The SVG masters, icons,
terminal logos and how to use them: [branding/](branding/README.md).

## Contributing

Contributions are welcome; read [CONTRIBUTING.md](CONTRIBUTING.md) first. In short: every commit is
signed off under the [Developer Certificate of Origin](DCO) (`git commit -s`); privileged code has no
`unsafe` and no dead code; every security or reversibility claim comes with a test that would fail if
it broke; and the documentation changes with the behaviour it describes.

## Licence

TorqOS is licensed under the [Apache License 2.0](LICENSE). The licence does not grant rights to the
TorqOS name or logo (its section 6); see [branding/](branding/README.md).

<p align="center"><br><img alt="TorqOS" src="branding/svg/mark/torqos-mark-small.svg" width="28"></p>
