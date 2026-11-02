# Hardware

TorqOS has no hardware of its own: it runs wherever NixOS runs (aarch64 and x86_64).

| Use | Machine |
|---|---|
| Dev | UTM ARM VM on an M4 Mac (`torq-utm`) |
| Tests | NixOS VM tests (`tests/`), QEMU |

Reference laptops, firmware and Secure Boot hardware guidance were dropped with the laptop goals
(ADR-019); the earlier list is in git history.
