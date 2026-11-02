# Security model

## Principles (now)

1. **The config repo is the system, and only the broker writes it.** Every change is a commit, a
   generation and a ledger entry; the ledger is hash-chained and verified.
2. **The AI can never touch the trust layer.** The ledger, the broker's settings, AI permissions
   and boot keys are user-only, changed by a person at a terminal.
3. **Thin.** Few services, no display server; what is not installed cannot be attacked.

## Later: owner-rooted boot security

The rest of this document (owner Secure Boot keys, measured boot, TPM2 + PIN disk unlock) is kept
for later, not planned now (ADR-019). It was written for laptops; on TorqOS it applies to any
machine with UEFI and a TPM.

1. **Each machine is its own root of trust.** Keys are generated on the machine at install.
2. **No vendor signatures required.** No shim, no Microsoft keys (default), no project-held
   signing key for boot. The project only signs its package repository/binary cache.
3. **Tamper → locked disk.** If the boot chain changes, the TPM refuses to release the disk key.

## Root, the owner, and an AI the owner runs

- **The `root` account is locked** (no password: nobody logs in as root). **Root's powers** are
  borrowed through `sudo` by the owner, with the owner's own password; `sudo bash` borrows them for a
  whole shell. That is the owner's override, as on any Linux, and TorqOS does not try to forbid it:
  people stay free, the AI is what is limited.
- **What a root shell can do:** go around the broker — edit the config repo or system files, rebuild
  by hand, rewrite the ledger. TorqOS's answer is that going around the door is never invisible:
  drift from the ledger raises `show system alarms`; `/etc/nixos` refuses a stray rebuild; the
  ledger's hash chain shows an edited entry. Gap: a root writer can rewrite the whole chain
  consistently — closed by signing entries with a key root cannot read and anchoring the head off
  the machine (KNOWN-GAPS #2, M3).
- **An owner who runs an AI tool in a root shell** has handed it root, and no software can stop
  that. TorqOS makes it **visible** (the same alarms; the ledger records what passed the door —
  but cannot yet tell a person from a program: the broker identifying its caller, KNOWN-GAPS #4,
  M3), **recoverable** (every configuration change undoes; `request system repair` returns the
  machine to its last ledgered state; the store is read-only and rebuilt from the configuration —
  but data a root process deletes, and a ledger it rewrites, need snapshots of data and the signed
  ledger, both later), and **unnecessary** (TorqOS's own AI layer: an unprivileged agent that reads
  and proposes, never `sudo`; the owner approves each proposal).
- **Strict mode (later, optional):** the owner's `sudo` may run only TorqOS's own commands — no
  root shell to run anything in — with a deliberate, ledgered break-glass for emergencies.
- **Dev mode** (the machine TorqOS is built on) is the labelled exception: passwordless `sudo` and an
  AI coding agent with root. Customer images ship with it off.

## Accounts and passwords (agreed for M2, 2026-11-01; built: INSTALL.md)

- A new image has no user account: only the locked `root`. **First-boot setup** creates the owner's
  account (name and password), asks how to log in remotely — **password, SSH key, or both** — and
  commits it as change #1. It never runs again.
- **Passwords never enter the config repo** (it is readable by everyone and kept forever; a hash
  there can be cracked offline): the configuration declares the account and points at a file only
  the system can read (NixOS `hashedPasswordFile`, outside the repo). A TorqOS command (e.g.
  `request system password`) changes it through the broker; the ledger records *that* it changed,
  when and by whom — never the secret. More users get theirs the same way. Built: the
  configuration tree refuses a person's password in any of NixOS's forms (`users users <name>
  password`, `hashedPassword`, `initialPassword`, `initialHashedPassword`, `hashedPasswordFile`,
  `passwordFile`), in torqcli and at evaluation (modules/settings-fence.nix, rule 3), saying to use
  `request system password <user>`.
- **Nothing listens on the network unless asked for:** no SSH until turned on, and no link-local
  name answering (systemd-resolved's LLMNR and multicast DNS, off: they let anyone on the same
  network answer for a name). tests/image.nix checks the image.
- **SSH is NixOS's own** (`services openssh enable true`; ADR-025), and TorqOS's protections apply
  however it is turned on (modules/services.nix): keys only unless
  `services.openssh.settings.PasswordAuthentication` is set, and then repeated wrong guesses are
  blocked (sshguard); 3 tries a connection; only the owner may log in (`AllowUsers`). These are
  defaults a person can change. `root` never logs in over SSH: an assertion refuses any
  `PermitRootLogin` but `no`. `show system alarms` notes that SSH accepts passwords. A commit (or
  revert) that changes who can log in over SSH — turning it on, allowing passwords or turning them
  off, removing a key, another owner — shows it (the keys that will be able to log in, or that
  passwords will be accepted), and is refused if SSH is keys only and there are none: with SSH
  already on as much as when it turns on (audit of 2026-11-01). Keys only stays one `set` away.
- **Releases are signed** (ADR-028): each image's checksum, with the TorqOS release key, and the one
  command that checks it (INSTALL.md); and each release's source, a git commit the key signed.
  A machine updates only to a commit signed with the key it was installed with
  (`/etc/torq/release-signers`): torqcli checks it before the update enters the candidate.

## Threat model

| Threat | Primary defence | Notes |
|---|---|---|
| Stolen machine | LUKS2 full-disk encryption | TPM2 + PIN, recovery key shown once at install |
| Evil maid (boot files tampered while away) | Owner-key Secure Boot + measured boot | Disk won't unseal if PCRs change |
| Firmware keys reset to factory | Firmware admin password; PCR 7 change → disk stays locked | `show system alarms` warns |
| Malware during a session | read-only /nix/store, few services, sandboxed services | |
| Malware persistence across boots | Signed UKIs only; config rebuilt from repo | Optional impermanence later |
| Prompt injection steering the AI | Broker tiers + taint tracking | Untrusted-content-derived actions need per-step approval |
| Compromised signing key on device | Only the db key lives on device (broker-only, TPM-sealed); PK/KEK exported offline then deleted | |
| Secure Boot bypass vulns (BlackLotus, LogoFAIL class) | fwupd firmware + dbx updates, applied via broker | |

## Secure Boot: owner keys, no Microsoft

Key hierarchy in firmware: **PK** (owner) → **KEK** → **db** (allowed) / **dbx** (revoked).

Install flow (later):

1. User puts firmware in **Setup Mode** (installer shows per-model instructions).
2. Generate PK/KEK/db on the machine (`sbctl create-keys`, wrapped by our installer).
3. Enroll keys **plus option-ROM hashes from the TPM event log**
   (`sbctl enroll-keys --tpm-eventlog`) — keeps GPU/TB/NVMe option ROMs working
   without trusting Microsoft.
4. Toggle (default OFF): "Keep Microsoft keys (needed to dual-boot Windows)" → `--microsoft`.
5. Prompt user to set a **firmware admin password**.
6. Export PK + KEK to USB / printed recovery sheet; delete from disk.
7. Reboot with Secure Boot on; Lanzaboote signs every generation's UKI with the db key.
8. Re-enroll LUKS to TPM2 + PIN bound to the measured boot state.

Before any firmware update the broker must check whether option-ROM hashes will change and
re-enroll first (otherwise: black screen risk).

Hardware guidance: integrated-graphics machines are lowest risk for removing Microsoft keys.

## Disk encryption

- LUKS2, `systemd-cryptenroll --tpm2-device=auto --tpm2-with-pin=yes`, PCR policy managed
  via Lanzaboote's measured boot / systemd-pcrlock support.
- Recovery key generated at install, shown once.

## Hardening defaults (initial list, expand with care)

- Kernel lockdown (integrity), signed modules only.
- No display server by default.
- Minimal enabled services; all TorqOS services use systemd sandboxing.
- `nix.settings.allowed-users` restricted; only the broker may trigger system rebuilds.

