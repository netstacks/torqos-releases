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
[Download the latest release](https://github.com/netstacks/torqos-releases/releases/latest){ .md-button }

</div>

## Where to start

<div class="tq-cards">
<a href="start/install/"><b>Install</b><span>Download an image, check its signature and boot it on your hypervisor.</span></a>
<a href="start/first-boot/"><b>First-boot setup</b><span>The few questions TorqOS asks once, and what it does with the answers.</span></a>
<a href="start/first-session/"><b>Your first session</b><span>Turn on SSH, add a package, compare, commit and roll back.</span></a>
<a href="reference/commands/"><b>Command reference</b><span>Every torqcli command on one page.</span></a>
</div>

## What makes TorqOS different

**One configuration tree.**
:   Every NixOS setting is a path in one tree, with `?` help, Tab completion and abbreviations.
    `show configuration` is the whole machine.

**A network device's commit model.**
:   Changes go into your private *candidate*. `show | compare` shows what will change,
    `commit check` builds it without switching, and `commit` applies it. `commit confirmed 5` rolls
    itself back unless you confirm it, which makes changing SSH or networking on a remote machine safe.

**Everything reversible.**
:   `rollback <n>` loads any earlier configuration. `request system revert #85` undoes one recorded
    change. The boot menu keeps every system version.

**A tamper-evident history.**
:   Every change is recorded in a hash-chained ledger: who asked, who approved, what changed, and the
    system it produced.

**Secure by default.**
:   Nothing listens on the network until you ask for it. `root` is locked. SSH, once on, is keys only
    unless you allow passwords, for the owner only, never for `root`.

**Thin.**
:   No desktop and no graphical stack. TorqOS adds only the configuration layer and leaves everything
    else to NixOS.

!!! note "Early releases"
    TorqOS is in early releases (`v0.1.x`). The configuration layer is complete and in daily use;
    the installable images, first-boot setup and signed releases are new. An ISO installer for
    physical machines is planned.
