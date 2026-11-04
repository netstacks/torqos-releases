# Hardware

TorqOS has no hardware of its own: it runs wherever NixOS runs (aarch64 and x86_64).

| Use | Machine |
|---|---|
| Dev | x86_64 VM on VMware ESXi (`torq5`): 8 vCPU, 64 GB, 1 TB, nested virtualisation on |
| Tests | NixOS VM tests (`tests/`), QEMU with KVM on that host |
| aarch64 | CI only, on GitHub's arm64 runners (ADR-030) |

`torq5` was itself installed from a TorqOS release image (the x86_64 VMDK, 2026-11-02), so the dev
machine is a normal install: everything done to it goes through its own broker and ledger.

Reference laptops, firmware and Secure Boot hardware guidance were dropped with the laptop goals
(ADR-019); the earlier list is in git history.
