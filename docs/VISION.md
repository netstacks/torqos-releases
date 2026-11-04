# Vision

**An operating system that is its configuration — and that an AI can be trusted with.**

## One sentence

A thin OS built on NixOS where one configuration, kept in a git repo and changed only through
`torqcli` and a small privileged broker, defines the whole machine; every change is reviewed,
ledgered and undoable — so a server is code in git, and a laptop is a machine an AI can drive
without anyone fearing what it did.

## Two faces, one base (ADR-034)

| | The server | The desktop or laptop |
|---|---|---|
| Who drives it | a sysadmin, at `torqcli` | a person, talking to an agent; the agent proposes, the person approves |
| What it is | a machine defined in code, in git, rebuilt anywhere from its repo (ADR-036) | the same base plus a desktop *configuration* (`torqos-desktop`), loaded like any configuration |
| What it is for | servers as code: every setting typed, every change a commit, a generation, a ledger line | deep, powerful AI access made safe: fix a driver, build an application, watch a desktop — recorded, reversible |

The base is the same thin skeleton (ADR-032); the desktop adds its size as a configuration, not
as the base. Nothing the server face needs is given up for the laptop face, and the other way round.

## What TorqOS is

| Goal | What we do |
|---|---|
| The configuration is the device | Like a router's config: one tree (`torqcli`, every NixOS setting) defines the system. `show configuration` is the machine (docs/CONFIG-IS-THE-DEVICE.md). |
| Small and controlled | torq is the configuration part of the OS: a Rust broker that is the only writer of the config repo, and `torqcli`, a J-style shell over it. Everything else is plain NixOS. |
| Everything reversible | Every change is a git commit, a NixOS generation and a ledger entry. `rollback`, `commit confirmed`, `request system revert`; data snapshots with them (ADR-038). |
| The AI fenced, people free | The agent is an account the kernel vouches for (ADR-035), unprivileged, and can only *propose*; tiers decide what needs a person; the person's home is theirs, and the agent acting for them is free there too. An AI can be allowed to try a driver fix *because* the OS can un-try it. |
| The machine is a URL | A config repo with a remote (ADR-036), secrets fenced out by construction, hardware never travelling: a new install pointed at the repo rebuilds the machine; with a backup and a sync account (ADR-038), the data too. |
| Thin | A skeleton: what is not needed is not installed. A desktop is a configuration on it. |
| Reproducible | The same config repo and lock build the same system on another machine. |
| Honest about upstreams | TorqOS is NixOS plus a control plane. What it owes nixpkgs, the GPL and Flathub, and what it depends on, is written down (ADR-039). |

## The AI, concretely

Three things a person should be able to say to their machine, each a VM test when built
(ADR-034):

- **"Fix my wifi."** The agent reads logs and hardware (doctor mode), proposes a system-tier change,
  the person approves with one answer; it is applied with `commit confirmed`, and reverts itself if
  the machine does not come back. No other desktop OS can let an AI try this safely.
- **"Build me an application for this."** The agent writes it in the person's home (free), builds
  it, installs it through the tree at user tier (ADR-037) — no approval, every step a ledger line,
  `rollback` undoes it.
- **"Watch while I try it."** Agent tooling over the compositor's screenshot and input protocols,
  in the person's session. Nothing of the broker's is needed, and nothing of the broker's is in
  the way.

## Explicit non-goals

- Laptop-*specific* design, touch- and touchpad-first; reference hardware, firmware work (TorqOS
  runs wherever NixOS runs). The desktop is a configuration, not hardware work.
- A smoothness target, theming, "complete OS" polish.
- Building a file-sync service, a package repository or a binary cache of our own: a sync service
  is declared, not built (ADR-038); mirrors when there are users to serve (ADR-039).
- Preventing root. The agent cannot get root; the owner can, but not silently (ADR-035).

## Later (kept, not planned now)

- Owner-rooted boot security (own Secure Boot keys, TPM2 + PIN): SECURITY-MODEL.md.
- Strict mode without break-glass — no root shell at all — once every task an owner needs is a
  torqcli command (ADR-035).
- A shared base configuration with per-host overlays, for fleets (ADR-036).
- Mirrors of nixpkgs and a caching proxy, when TorqOS has users (ADR-039).
