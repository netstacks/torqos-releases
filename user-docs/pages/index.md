---
hide:
  - navigation
---

<div class="tq-hero" markdown>

# The configuration is the device.

<p class="lede">TorqOS is a thin, command-line operating system built on NixOS. You configure the
whole machine the way you configure a router: in a private candidate, compared before you commit,
recorded in a tamper-evident ledger, and reversible with one command.</p>

[Install TorqOS](start/install.md){ .md-button .md-button--primary }
[Download the latest release](https://github.com/torqosorg/torqos-releases/releases/latest){ .md-button }

</div>

## Where to start

<div class="tq-cards">
<a href="start/install/"><b>Install</b><span>Download an image, check its signature and boot it on a hypervisor, or install it on a real machine from a USB stick.</span></a>
<a href="start/first-boot/"><b>First-boot setup</b><span>The few questions TorqOS asks once, and what it does with the answers.</span></a>
<a href="start/first-session/"><b>Your first session</b><span>Turn on SSH, add a package, compare, commit and roll back.</span></a>
<a href="reference/commands/"><b>Command reference</b><span>The torqcli commands, on one page.</span></a>
</div>

## What makes TorqOS different

**One configuration tree.**
:   Every NixOS setting is a path in one tree, with `?` help, Tab completion and abbreviations.
    `show configuration` is the whole machine.

**A network device's commit model.**
:   Changes go into your private *candidate*. `show | compare` shows what will change,
    `commit check` evaluates it (it checks the configuration but does not build it), and `commit`
    builds and applies it. `commit confirmed 5` rolls
    itself back unless you confirm it, which makes changing SSH or networking on a remote machine safe.

**Everything reversible.**
:   `rollback <n>` loads any earlier configuration. `request system revert #85` undoes one recorded
    change. The boot menu keeps every system version.

**A tamper-evident history.**
:   Every change is recorded in a hash-chained ledger: who asked, who approved, what changed, and the
    system it produced. Against someone with root on the same machine this holds only if the
    configuration is also pushed to a remote, which is off by default (see
    [Security defaults](reference/security.md)).

**Secure by default.**
:   Nothing listens on the network until you ask for it. `root` is locked. SSH, once on, is keys only
    unless you allow passwords, for the owner only, never for `root`.

**Thin.**
:   The base has no desktop and no graphical stack. TorqOS adds only the configuration layer and
    leaves everything else to NixOS. A desktop (COSMIC) is available as an opt-in configuration:
    see [The desktop](guide/desktop.md).

!!! note "Early releases"
    TorqOS is in early releases; the latest is `v0.3.2`. Its owner has used the configuration
    layer daily since 2026-10-30, and no one outside the project has tested it yet. Images,
    first-boot setup, signed releases and an ISO installer for physical machines are available.
    The desktop and the agent are experimental; their pages say how far they go. A `0.4.0` release
    candidate is in preparation and is not released.
