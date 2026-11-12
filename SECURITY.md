# Security policy

TorqOS's claims are written down: [docs/SECURITY-MODEL.md](docs/SECURITY-MODEL.md) says what it
protects and how; [docs/KNOWN-GAPS.md](docs/KNOWN-GAPS.md) and the latest audit
([docs/AUDIT-2026-11-06.md](docs/AUDIT-2026-11-06.md)) say what is known not to hold yet. A way
around a claim that is not already listed there is a vulnerability — please report it.

## Reporting

Report privately, through GitHub: **[Report a vulnerability](https://github.com/torqosorg/torqos/security/advisories/new)**
(the repository's Security tab). Please do not open a public issue or pull request for it.

Say what you did, what you expected, and what happened — the TorqOS version
(`torqcli show version`, or `/etc/os-release`) and, if you can, the commands or the
configuration change that shows it. You will hear back within a week. A fix is released as a new
version (machines on `stable` take it as a signed update, ADR-028), and the advisory is published
with it, crediting you unless you would rather not be named.

## Supported versions

TorqOS is young and has one supported line: the latest release, the one `stable` points to.
Fixes are not backported; a machine updates to the release that carries them.

| Version            | Supported |
| ------------------ | --------- |
| latest (`stable`)  | yes       |
| anything older     | no — update |

## Especially wanted

- A way for an unprivileged account (the agent's, which is experimental) to change the system without the
  broker, or to change the broker's ledger without `torq-broker verify` noticing.
- A configuration change that slips past the settings fence (`modules/settings-fence.nix`).
- A release a machine accepts that the release key did not sign.
- A change that cannot be rolled back, or a rollback that leaves the machine other than the ledger says.
