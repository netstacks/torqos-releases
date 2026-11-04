# Driving TorqOS: the owner's checklist

This is for the person steering AI sessions, not for the sessions. It says what to do next, when
to stop, and when to hand a session's work to a *different* session to check. The plan itself is
`docs/ROADMAP.md`; the reasons are `docs/DECISIONS.md`. This is the procedure.

Three rules that cover most of what goes wrong:

1. **One session writes in a tree at a time.** Two sessions in `~/torq` overwrite each other.
   A second piece of work gets its own worktree and branch (`git worktree add ~/torq-<name> -b
   <name>`), as `~/torq-slim` does.
2. **The session that built it never judges it.** A builder will tell you it works. A fresh
   session, given only the diff and the ADR — not the builder's summary — tells you whether it does.
3. **Nothing ships from a dirty tree.** `git status` before `request system update` on torq5: a
   `path:` source builds whatever is on disk, committed or not.

## The loop, for one piece of work

Every ROADMAP checkbox goes through this. Do not skip the stops; they are the point.

```
 pick ──► brief ──► BUILD ──► stop ──► VALIDATE ──► fix? ──► ship ──► record
```

### 1. Pick
One unchecked line of `docs/ROADMAP.md`, in the current milestone. If it has no ADR, or the ADR is
"proposed" and you have not decided, the first session's job is the ADR, not code. Decide, then
build. Never both in one session.

### 2. Brief the build session
Give it: the ROADMAP line, the ADR number, what "done" looks like, and which test must exist. A
usable brief fits in six lines:

```
Build ROADMAP M3 "the broker as a socket service" per ADR-035 lever 1.
Done when: torqcli runs without sudo; a system-tier commit asks my password;
`--actor`/`--approved-by` are no longer accepted from the caller.
Tests: a VM test in tests/ that fails if a caller can name its own actor.
Keep: the sudo broker for dev mode and recovery. Update KNOWN-GAPS 4 and 6a.
Branch: m3-socket, worktree ~/torq-m3. Don't touch ~/torq.
```

### 3. Build
Let it work. Interrupt only for a decision that is yours (the ADR's open questions). Before it
ends, it must have, in this order — ask for each by name if it stops early:

- [ ] the tests it was told to write, and `nix flake check` green (`cargo test` for the crates)
- [ ] docs touched: the ADR's status, KNOWN-GAPS lines it closed, CLI-DESIGN if a command changed
- [ ] one or more commits, `git commit -s`, each message saying *why*
- [ ] `git status` clean in its worktree

### 4. STOP
Close the session, or leave it. Do not ask it "is this good?" — it will say yes.

### 5. Validate, in a fresh session
A new session, no history. Give it the branch and the ADR. Do not give it the builder's summary.
Two kinds, use both for anything in the broker, the fence, setup, the installer or SSH:

**Review** — "Review branch `m3-socket` against ADR-035. For each claim the ADR makes, name the
test that would fail if the claim were false, or say there is none. List what the diff changes
that the ADR does not mention." Use `/code-review` for the mechanical pass; the prompt above for
the design pass.

**Adversarial** — "You are root on a machine running branch `m3-socket`. Get a change applied
with a false actor. Get a system-tier commit through without a password. Show the commands." If
it finds a way, that is a KNOWN-GAPS entry or a fix — never a shrug.

What comes back is either *nothing* (ship), *a fix* (back to step 3 with the finding as the brief,
same branch), or *a design hole* (back to step 1: the ADR was wrong, amend it first).

### 6. Ship
Merge to `main` (fast-forward if you can). On torq5, in tmux, with a private candidate:

```bash
cd ~/torq && git status            # clean, on main
torqcli -c "request system update; configure; show | compare; commit comment \"M3: the socket broker\""
```

Read the prompt: `[edit system software]` only. If the machine misbehaves: `rollback 1`.
Push `main`; CI runs both architectures — aarch64 is only tested there (ADR-030).

### 7. Record
Tick the ROADMAP line, with the date and the test's name in italics as the others are. If the
"Where things stand" section of `CLAUDE.md` is now wrong, fix it in the same commit. Then step 1.

## When to run a whole-project audit

Not per piece of work. Run one — a fresh session, "audit the broker and the fence as the two
audits of 2026-11-01 did; write `docs/AUDIT-<date>.md`" — at these points:

- the end of each milestone, before the next starts
- before any release that real people will install
- after anything that changes *who can do what*: the socket, strict mode, the remote, the agent

Its findings go into the KNOWN-GAPS table with a status, as the last ones did. They are fixed
when the work they belong to comes up, not all at once.

## The milestones, with the stops marked

Each item: what to build, the test that must exist, and **⏹** where a fresh session validates
before you go on. A ⏹ with *adversarial* means both kinds.

### M2 — finish (ADR-032, ADR-033, ADR-039)
- [x] Merge `slim` after the installer lands (2026-11-03; reviewed: no race, three fixes made). *Test:* `checks.x86_64-linux.size` under budget;
      trust-loop and image tests green with the `nix`-only switch.
      **⏹ review:** does the broker's new switch path race `nixos-rebuild`'s unit as the old one did?
- [x] Decide ADR-033: Lanzaboote now, Secure Boot later (2026-11-03).
- [x] The EFI-partition VM test **first**, against today's Python writer: entries after a switch,
      rollback by choosing an older entry, a `commit confirmed` rolled back by a reboot, the editor
      off. *(tests/boot.nix, check `boot`, 2026-11-03; mutation-checked with the editor on.)*
- [~] Lanzaboote (ADR-033): built on a branch (deleted 2026-11-04; its lessons in the ADR), then **deferred** (2026-11-03, the owner):
      Python stays for growpart anyway, so it saved nothing, and its reviews found the move unsafe
      as built (a full EFI partition, a clean-up that can remove working entries). It comes with
      Secure Boot (M4 or later); the reviews' findings are in ADR-033's amendment.
- [x] ADR-039 (2026-11-03, `checks.<system>.free`): the source statement in `release.yml`; a CI check that the image and ISO hold
      nothing nixpkgs marks unfree. *Test:* the check fails when `claude-code` is put in the image on purpose.
- [x] **⏹ audit** — end of M2 (2026-11-04, `docs/AUDIT-2026-11-04.md`: no high finding; A1, A4, A12 fixed, A3 and A11 in part, the rest in KNOWN-GAPS). Then a release (`v0.2.0`) from a machine tracking `stable`.

### M3 — trust (ADR-035, ADR-036)
Order matters: the socket first, because everything after keys on identity.
- [ ] The broker as a socket service; `SO_PEERCRED`; torqcli without sudo; the owner's password
      for system tier. *Test:* a VM test that a caller cannot name its actor or approver.
      **⏹ adversarial.**
- [ ] The agent's account `torq-agent` and the allow-list at the broker. *Test:* `torq-agent`
      proposing a system path, a config file, a `users/<other>` file — each refused and ledgered.
      **⏹ adversarial.**
- [ ] The remote: settings, push after apply, ledger as a ref, `verify` against it, `request
      system pull` as a proposal. *Test:* a VM with a bare remote: a rewritten local ledger is an
      alarm; a remote commit lands in the candidate and nowhere else. **⏹ review:** nothing from
      the remote is ever applied unasked — find the path that would.
- [ ] Adopt at setup. *Test:* two VMs: the second, pointed at the first's repo, is the first
      (same tree, its own hardware file, its own password). **⏹ review.**
- [ ] Strict mode and `torq break-glass`. *Test:* a shell leaves a ledger entry before it opens;
      `sudo bash` is refused. **⏹ adversarial:** get root without an entry.
- [ ] Snapshots before each commit (needs the installer's btrfs choice). *Test:* a file deleted
      by a commit's activation is back after `rollback 1`.
- [ ] **⏹ audit** — end of M3. This is the audit that matters most: after it, the security claims
      in SECURITY-MODEL should each name a test.

### M4 — the desktop as a configuration (ADR-034, ADR-037, ADR-038)
Can run in its own worktree beside M3; it is a configuration, not broker work.
- [ ] `torqos-desktop`: niri, terminal, launcher, portal, Flatpak. *Test:* a VM boots it to a
      session; `load override` on an image machine produces it. **⏹ review:** does the
      configuration carry anything of the base's that should be a setting?
- [ ] `users <name> flatpaks`. *Test:* declared → present after switch; removed → gone.
- [ ] Rung 3, a person's own builds in the tree — its design first (one session, an ADR
      amendment), then code. *Test:* a package from a repo in `/home` installs at user tier and
      `rollback` removes it. **⏹ adversarial:** can a user-tier build run anything as root?
- [ ] Data: `services syncthing`, `services restic backups`, `request system restore`. *Test:* a
      backed-up file returns on a rebuilt VM. **⏹ review:** the backup key is nowhere in the repo.
- [ ] Approval as a dialog. *Test:* the dialog's text is torqcli's prompt text (one source).
- [ ] **⏹ audit** — light: the desktop's attack surface (what listens, what the portal grants).

### M5 — the agent (AI-LAYER.md)
Only after M3's audit. The agent's safety is M3's; do not build it on the sudo broker.
- [ ] Doctor mode. *Test:* the agent's tools cannot write — a VM test that every tool is read-only
      against the fence and the repo. **⏹ adversarial:** prompt-inject the doctor into proposing.
- [ ] Proposals. *Test:* scenario (a), "fix my wifi": the proposal, the approval, `commit
      confirmed`, the revert when the machine does not confirm. **⏹ adversarial.**
- [ ] Session trust. *Test:* it never covers system tier; it expires; each change is ledgered.
      **⏹ adversarial.**
- [ ] "Build me an application" (scenario b) and "watch while I try it" (scenario c). *Test:* each
      end to end in a VM. **⏹ review.**
- [ ] **⏹ audit** — full, before anyone outside the project runs an agent.

## Signs a session is off track

Stop it when you see one of these; brief again, narrower.

- It is changing a document to match the code instead of the code to match the document.
- It added `#[allow(…)]`, an `unsafe` block, or weakened a default to make a test pass (ADR-020;
  CLAUDE.md guardrails).
- It is touching files the brief did not name — especially `modules/settings-fence.nix`,
  `broker/src/policy.rs`, `modules/security.nix`, anything under `hosts/`.
- It says a test is "not needed" or "covered elsewhere" for a trust claim.
- It wrote a new ADR you did not ask for. Decisions are yours; sessions propose them.
- It is reporting success without having run `nix flake check` or the VM test it was told to write.
- The diff is large and the commit message is short.

## Standing commands

```bash
nix develop --command bash -c 'cd broker && cargo test'     # the broker
nix develop --command bash -c 'cd cli && cargo test'        # torqcli
nix flake check                                             # everything, x86_64
nix build .#checks.x86_64-linux.<name>                      # one VM test
sudo torq-broker status && sudo torq-broker verify          # the machine is as the ledger says?
torqcli "show system alarms"
torqcli "show system ledger | tail 20"
torqcli -c "rollback 1; commit comment \"undo\""            # back one change
```
