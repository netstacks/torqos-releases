# Development setup

TorqOS is developed on **`torq5`**, an x86_64 VM on a VMware ESXi server (ADR-030). Everything day
to day happens there: the Rust crates, `nix flake check`, the VM tests and the images. **aarch64 is
CI's** — GitHub's arm64 runners build both architectures and run the tests; there is no aarch64
machine to develop on. (The earlier aarch64 UTM VM on an M4 Mac is retired; its instructions are in
git history.)

The dev machine is a **normal TorqOS install**, not a hand-built one: it was installed from a
release image, so every change to it goes through its own broker and ledger. Dev mode and the local
source are two ledgered, reversible commits on top — not a different kind of machine.

## The machine

### 1. The ESXi VM

| | |
|---|---|
| Firmware | UEFI |
| CPU | 8 vCPU, **"Expose hardware assisted virtualization to the guest OS" on** |
| Memory | 64 GB |
| Disk | 1 TB (thin is fine) |
| Network | one adapter, reachable by SSH |

Nested virtualisation is the setting that matters: without it there is no `/dev/kvm` and every
NixOS VM test falls back to QEMU's TCG emulation — minutes instead of seconds. It can only be
changed with the VM powered off. Afterwards:

```bash
ls -l /dev/kvm                 # must exist
grep -o -m1 vmx /proc/cpuinfo  # must print vmx
```

The disk needs no partitioning work: the image's `hardware-configuration.nix` sets
`autoResize` and grows its last partition, the root, with systemd-repart in the initrd (ADR-032), so the root filesystem grows to fill whatever disk the VM
is given on first boot.

### 2. Install TorqOS from a release image

Follow `docs/INSTALL.md`: download `torqos-x86_64-linux.vmdk.gz` from the release, check its
signed checksum, `gunzip` it, attach it as the VM's disk and boot. First-boot setup asks at the
console for the host name, time zone, the owner's account and password, and how to log in
remotely; that becomes change #1 in the ledger. No default password ships.

Nothing here is special to developing TorqOS — this is the install a user gets, which is the point:
bugs in it show up on the dev machine first.

### 3. Get the repo

```bash
git clone https://github.com/netstacks/torqos.git ~/torq
```

`~/torq` whatever the repository is called: the docs, the `torq` input below and the commands in
`CLAUDE.md` all use that path.

### 4. Turn it into a dev machine — two ledgered commits

Both are system-tier changes, so both ask at a terminal and both are one `rollback` away. Order
matters: `modules/dev.nix` asserts that a `path:` source outside the Nix store requires dev mode,
so dev mode goes first.

```bash
# a) Dev mode (ADR-009): passwordless sudo for wheel, the owner a trusted Nix user,
#    claude-code + gh + tmux + vim installed.
torqcli -c "configure; set torq dev enable true; show | compare; commit comment \"dev mode on (ADR-030)\""

# b) Build TorqOS from the checkout instead of the signed releases repo.
torqcli -c "configure; set system software torq source path:/home/viper/torq; show | compare; commit comment \"TorqOS from ~/torq\""
```

After (b) the machine builds the **working tree** at `~/torq`, uncommitted changes included: a
`path:` flake input copies the directory as it is on disk, it does not read git. So a dirty tree
becomes part of the system the broker ledgers — the change is recorded and reversible, but its
source is not reproducible from the repository. Commit before shipping a change, and check
`git status` first if you care which code you are running.

This is also the trade: while its source is a path, `torq5` is no longer exercising the release
path real machines take, so a release still has to be checked from a machine tracking `stable`
(ADR-028).

Dev mode is a deliberate exception for *building* TorqOS. It gives an AI coding agent full root and
a way around the broker. Never enable it on a real install, and do not design the TorqOS agent
around having root: the agent itself stays unprivileged and broker-only.

### 5. Identity and credentials

```bash
git config --global user.name "Your Name"
git config --global user.email "you@example.com"
gh auth login                       # gh arrives with dev mode; the remote is HTTPS
```

Contributions are signed off under the DCO (ADR-027), so commit with `-s` and use a real name:
`git commit -s -m "..."`. CI checks the sign-off on every pull request.

### 6. The dev tools

The Rust toolchain is **not installed into the system** — it comes from the flake's dev shell, so
the machine stays thin and nothing about the toolchain is a ledgered change:

```bash
cd ~/torq && nix develop          # cargo, rustc, clippy, rustfmt, rust-analyzer, nixfmt
```

Run long jobs inside tmux (`tmux new -A -s claude`), especially anything that rebuilds the system:
a `switch` restarts sshd, and a dropped connection would otherwise kill the run.

## Everyday work

### The loop

```bash
cd ~/torq
nix develop --command bash -c 'cd broker && cargo test'   # the broker's unit tests
nix develop --command bash -c 'cd cli    && cargo test'   # torqcli's
nix develop --command bash -c 'cd agent  && cargo test'   # the agent's doctor mode (a scripted model)
nix flake check                                           # evaluation, tree checks, VM tests
nix build .#checks.x86_64-linux.trust-loop                # one VM test
nix build .#checks.x86_64-linux.image                     # the image test: boot, setup, update
nix build .#image                                         # the qcow2; .#image-vmdk for ESXi
nixfmt-rfc-style modules/*.nix                            # formatting
```

`nix flake check` only covers this machine's architecture. aarch64 is checked by CI — assume
nothing about it locally.

### Shipping a distribution change to this machine

Commit in `~/torq` first, then move the machine's `flake.lock` forward through the broker so the
change is ledgered. Use a **private candidate** (`-c`): the interactive candidate is shared by
every interactive session of the user and may hold someone's uncommitted edits.

```bash
torqcli -c "request system update; configure; show | compare; commit comment \"…\""   # asks y/N
```

Read the prompt before answering: it must show only `[edit system software]`. If it shows
`[edit system software torq]` it is about to change *where TorqOS comes from*, which is not what
updating the lock should do.

To change the machine's config repo itself rather than its inputs, go through the broker by hand:

```bash
rm -rf /tmp/cfg && cp -r /var/lib/torq/config /tmp/cfg && cd /tmp/cfg   # edit, then:
git diff > /tmp/change.diff
sudo torq-broker propose --diff /tmp/change.diff --reason "..." --actor user
sudo torq-broker apply <seq> --approved-by $USER
sudo torq-broker status        # is the machine as the ledger says?
sudo torq-broker log && sudo torq-broker verify
```

There is no `nixos-rebuild` on TorqOS (ADR-032): the broker builds and switches with `nix` itself.
Do not build `/etc/nixos` with `nix` either: on TorqOS those files are stubs that refuse to build
(`torq.broker.guardEtcNixos`), because building them would replace the whole system outside the
ledger. Never build any configuration other than the broker's.

### torqcli

Everything else is `torqcli` (`torq` is the same program), driven like a network device (ADR-016,
`docs/CLI-DESIGN.md`): edit a candidate, look at it, commit it as one ledgered change.

```bash
torqcli                                  # the shell; `?` and Tab say what may come next
torqcli show packages ripgrep            # one command, from your shell (* marks what is installed)
torqcli -c "configure; set users users $USER packages ripgrep; commit"
#   set environment systemPackages <name>   # for everyone (system tier: asks at a terminal)
#   delete users users <you> packages <name>
torqcli show configuration
```

History and undo:

```bash
torqcli show system commit               # the configurations, numbered for rollback
torqcli show system ledger               # every change, refused and failed ones too
torqcli request system revert '#23'      # undo one change (a change forward; asks unless it is your own)
torqcli -c "configure; rollback 2; commit"   # back to a whole earlier configuration
torqcli show system alarms               # is the machine as its ledger says? (request system repair)
```

Services are NixOS's own settings, with TorqOS's protections applied however they are turned on
(e.g. SSH: keys only, only the owner, no root login; `modules/services.nix`). Always system
changes, so always confirmed:

```bash
torqcli show services                    # what the configuration turns on
torqcli -c "configure; set virtualisation podman enable true; commit"   # rootless containers
```

SSH is off on a new machine unless setup turned it on. To turn it on later, put your public key in
`~/.ssh/authorized_keys` at the console, then `set services openssh enable true` and commit: it
shows exactly which keys will be able to log in, and refuses if there are none.

`show system alarms` says whether the machine is as its ledger says; it is also checked at boot and
every 15 minutes, and a new shell warns if not.

## Throwaway VMs (nothing to install)

```bash
nix build .#nixosConfigurations.dev-vm-x86_64.config.system.build.vm
./result/bin/run-torq-dev-vm      # login: dev / torq
```

Fast here, now that KVM is available.

## A local model (doctor mode, ADR-042)

Off by default. On a machine, the owner turns it on in the tree, naming a model file (GGUF) by its
URL and hash — Nix fetches it when the change is built; nothing is downloaded otherwise:

```
configure
set torq agent doctor enable true
set torq agent model url https://…/model.gguf
set torq agent model hash sha256-…
commit
torq doctor "why is my wifi flaky?"
```

llama-server then runs on `/run/llama-cpp/llama.sock`; 64 GB here is enough for the models in
question. The journal is off unless `set torq agent doctor journal true` (then every program run as
the agent reads it). A remote endpoint instead: `set torq agent doctor endpoint https://…/v1` —
what the tools return, the configuration included, then leaves the machine — its key in a
file outside the repo (`torq.agent.doctor.apiKeyFile`). The doctor's unit tests need no model:
`nix develop --command bash -c 'cd agent && cargo test'`.
