# AI layer

**Next (ADR-034): M5, on the account M3 makes (ADR-035) and the desktop configuration M4 makes.**
Everything built so far (the broker as the only writer, actors, tiers, the ledger) was built so
this could be added without a bypass. The agent is an *account*, `torq-agent`, whose identity the
kernel vouches for (`SO_PEERCRED`) — never a flag the caller sets.

## What it is for

Three things a person says to their machine; each is an acceptance test (ROADMAP M5):

| | The agent does | The gate |
|---|---|---|
| "Fix my wifi." | reads logs and hardware (doctor), proposes a system-tier change | the person's one answer; `commit confirmed`, so it reverts itself if the machine does not come back |
| "Build me an application for this." | writes and builds it in the person's home, installs it through the tree (ADR-037, rung 3) | none: user tier; every step a ledger line; `rollback` undoes it |
| "Watch while I try it." | screenshots and input through the compositor's protocols | the person's session; nothing of the broker's |

In order: **doctor mode** (read, explain, propose nothing; built, ADR-042: `torq doctor "<question>"`), **proposals** (built: `torq agent propose "<ask>"`), **session trust** (built: `request system agent trust <minutes>`; "for
the next hour, user-tier changes without asking" — ledgered, revertible, never for system tier).
On a desktop the approval is a dialog with the owner's password, the one thing a program in the
owner's shell does not have; in a terminal it is torqcli's prompt, the same text.

## Components

| Component | Privilege | Language | Role |
|---|---|---|---|
| `torq-agent` (`agent/`, `torq-doctor`) | unprivileged, sandboxed (`torq agent run`) | Rust | Talks to the model (OpenAI-compatible chat API), calls its read-only tools; later *proposes* changes (ADR-042) |
| MCP tool servers | unprivileged, read-mostly | Rust | Typed access per subsystem |
| `torq-broker` | privileged (root, minimal) | Rust | Only writer of the config repo; tiers, commits, rebuild, rollback, ledger |
| Model runtime | unprivileged (DynamicUser) | llama.cpp (`llama-server`, a unix socket) | Local inference; or a remote endpoint as a setting (ADR-042) |
| torqcli | the person's | Rust | Shows proposals, diffs and the ledger; a person approves at a terminal |

## Permission tiers

| Tier | Examples | Requirement |
|---|---|---|
| `observe` | read logs, hardware info, settings, config repo | automatic, still ledgered |
| `user` | one user's *data* files: today `users/<name>/packages.json` (apps); later themes, dotfiles. Never a `.nix` module (that is `system`, wherever it lives) | approve once per category; /home snapshot first |
| `system` | services, drivers, kernel params, packages | always preview + explicit approval; commit + generation |
| `forbidden` | AI permissions, ledger, Secure Boot keys, firmware password, disabling audit | **never** via agent; user-only, at a terminal |

## Taint tracking

Any agent session that has ingested untrusted content (logs, device names, the ledger, configuration
values; web pages, email, downloaded files) is marked **tainted**. Tainted sessions cannot use
"approve once" shortcuts: every `user` or `system` action requires explicit per-step approval
with the taint source displayed.

Built (ADR-042 6, amended): the taint is the **broker's**. TorqOS's agent starts its session at the
broker and reports each tainting read there before the model sees it; the broker keeps the record in
its own state directory, where the agent cannot write, and nothing clears it. The ledger holds a
session where a proposal cites it: the proposal's entry carries the session and what tainted it.
A proposal names its session and is tainted as the broker's record says;
one that names no session — any other program run as the agent — is tainted. `request system apply`
says what the session read; session trust never covers a tainted proposal. The broker holds what it
is told, and cannot see what a program reads: KNOWN-GAPS 6zq.

## Ledger format (v0)

Append-only JSON Lines at `/var/lib/torq/state/ledger.jsonl`, owned by the broker.
Entry kinds: `init`, `proposal`, `applied`, `apply_failed`, `reverted`, `revert_failed`,
`interrupted`, `repaired`, `confirmed`, `rolled_back` (an unconfirmed `commit confirmed`), and a
person's `trust_granted`, `trust_revoked`, `trust_expired`
(failures and restores carry `restored: true|false` — see ADR-008).

```json
{"seq":1042,"ts":"2026-10-30T15:20:11Z","actor":"agent","approved_by":"casey",
 "kind":"applied","tier":"user","tainted":false,
 "reason":"User asked for night light at sunset",
 "data":{"proposal":1041,"commit":"8f3a2c1…","files":["desktop.nix"]},
 "prev_hash":"…","hash":"…"}
```

- `hash = sha256(prev_hash || canonical_json(entry_without_hash))`
- Genesis entry has `prev_hash` = 64 zeros.
- `torq-broker verify` recomputes the chain and reports the first broken link.
- Git commits carry a `Ledger-Entry: <seq>` trailer; the ledger stores the commit id → each
  checks the other. TODO(M3): `verify` does not cross-check trailers yet, and cannot detect
  truncation of the newest entries until the head hash is anchored.
- Later (M3+): anchor the latest hash periodically (e.g. TPM NV index or an external witness).

## Change lifecycle

1. `propose` — agent or user submits a unified diff against the config repo + a reason.
2. Broker validates: diff applies cleanly, touches only allowed paths for its tier, tier ≤ allowed.
   Paths are what the diff actually changes (applied to a scratch index): both sides of a
   rename, raw names — so renames, deletions and quoted names cannot dodge the forbidden tier.
3. Preview shown; approval collected (torqcli at a terminal; the broker takes `--approved-by`).
4. `apply` — snapshot data (ADR-038), commit, build and switch, ledger append, push and anchor
   (ADR-036). (Signing on the machine is not the defence — ADR-035; the anchor is.)
5. `rollback <seq|commit>` — `git revert`, rebuild, ledger append.

## Tool inventory

Built (doctor mode, ADR-042, `agent/src/tools.rs`; all read-only): `read_journal`,
`hardware_facts`, `show_configuration`, `show_ledger`, `show_alarms`, `nixos_option_docs`. All but
the option docs return untrusted text (a configuration value too) and
taint the session. In propose mode (ADR-042 6): `propose_change`, structured words of the tree, a
proposal through torqcli as the agent.
Later: `logs.coredump`, `pkg.search`, `power.status`, `secureboot.status`, `files.read_scoped`.
