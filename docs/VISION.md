# Vision

**An operating system that is its configuration.**

## One sentence

A thin, command-line-only, fully functional OS built on NixOS, where one configuration, kept in
a git repo and changed only through `torqcli` and a small privileged broker, defines the whole
machine, and every change is reviewed, ledgered and undoable.

## What TorqOS is

| Goal | What we do |
|---|---|
| The configuration is the device | Like a router's config: one tree (`torqcli`, every NixOS setting) defines the system. `show configuration` is the machine (docs/CONFIG-IS-THE-DEVICE.md). |
| Small and controlled | torq is the configuration part of the OS: a Rust broker that is the only writer of the config repo, and `torqcli`, a J- or C-style shell over it. Everything else is plain NixOS. |
| Everything reversible | Every change is a git commit, a NixOS generation and a ledger entry. `rollback`, `commit confirmed`, `request system revert`. |
| Ready for AI, later | No AI now. Built so AI layers can be added later without bypass: the broker is the only writer, every change names its actor, tiers decide what needs a person. Then an AI could only *propose*; people stay free. |
| Thin | CLI only. No desktop, no UI layer: what is not needed is not installed. |
| Reproducible | The same config repo and lock build the same system on another machine. |

## Explicit non-goals (for now)

- A desktop, a graphical shell or any UI beyond the terminal.
- Being a laptop OS, or touch- and touchpad-first design.
- Hardware-specific work (reference machines, firmware, Secure Boot key ceremonies): TorqOS runs
  wherever NixOS runs; today, the dev VM.
- A broad "complete OS" vision: opinionated apps, theming, smoothness targets.

## Later (kept, not planned now)

- The AI layers (docs/AI-LAYER.md): an agent that reads the system and *proposes* changes through
  the broker, limited permanently and without bypass. Everything built now keeps that possible.
- Owner-rooted boot security (own Secure Boot keys, TPM2 + PIN): SECURITY-MODEL.md.
- **Far future:** the config repo with a private git remote. At install, TorqOS asks for a
  GitHub/GitLab (or other) account and permission to create a private repo, and uses it as the
  config repo's remote, so the system's settings are a true git repo off the machine too. To
  settle first: push-only or also accepting changes from the remote (never bypassing local
  approval), and where the credential lives so an AI can never use it.
