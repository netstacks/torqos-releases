# Development setup

## On the M4 Mac (primary)

### 1. UTM VM
1. UTM → Create → **Virtualize** → **Linux**. Leave "Use Apple Virtualization" **off** (QEMU).
2. Boot ISO: `latest-nixos-graphical-aarch64-linux.iso` (verify SHA-256).
3. 8 GB RAM (16 if available), 4+ cores, 64 GB disk. Hardware OpenGL **off** for install.
4. Install with the graphical installer. Any (lowercase) user name works; setup below uses `$USER`.
   Desktop choice doesn't matter: TorqOS is CLI only.
5. Shut down, remove the ISO from the CD drive, boot the installed system.

### 2. Bootstrap inside the VM

> Prerequisite: `broker/Cargo.lock` must be committed in the TorqOS repo (see CLAUDE.md →
> "First tasks"), otherwise the broker package — and so the system — won't build.

```bash
# a) Enable flakes + SSH once, the classic way:
sudo nano /etc/nixos/configuration.nix
#   nix.settings.experimental-features = [ "nix-command" "flakes" ];
#   services.openssh.enable = true;
#   environment.systemPackages = [ pkgs.git ];
sudo nixos-rebuild switch

# b) Get the TorqOS repo
git clone <your GitHub repo URL> ~/torq

# c) Create this machine's config repo from the template
sudo nix flake new -t ~/torq#config /var/lib/torq/config
sudo cp /etc/nixos/hardware-configuration.nix /var/lib/torq/config/   # attached at the top (config-files.json)
sudo sed -i "s/CHANGE-ME/$USER/" /var/lib/torq/config/nixos/torq.json    # the one place a user name is set
sudo sed -i "s|github:netstacks/torqos|path:$HOME/torq|" /var/lib/torq/config/flake.nix  # dev: your checkout

# (In a VM, optionally, from torqcli after the first boot: set services qemuGuest enable true,
#  set services spice-vdagentd enable true. On x86_64, set `system = "x86_64-linux"` in flake.nix.)

# d) First switch into TorqOS (explicit host name the first time)
sudo nixos-rebuild switch --flake /var/lib/torq/config#torq
sudo reboot
```

After reboot, `torq-broker-init` has turned `/var/lib/torq/config` into a git repo with a
ledger. From now on, **all** changes go through the broker:

```bash
# make a change in a scratch copy and produce a diff:
sudo cp -r /var/lib/torq/config /tmp/cfg && sudo chown -R $USER /tmp/cfg
printf '{ "printing": { "enable": true } }\n' > /tmp/cfg/nixos/services.json   # (if it does not exist yet)
(cd /tmp/cfg && git diff > /tmp/printing.diff)
sudo torq-broker propose --diff /tmp/printing.diff --reason "Enable printing" --actor user
sudo torq-broker apply <seq> --approved-by $USER      # commits + nixos-rebuild switch
sudo torq-broker log
sudo torq-broker revert <applied-seq> --approved-by $USER
sudo torq-broker verify
```
(Producing diffs by hand is the low-level way: torqcli makes them for you, and the agent will propose them in M2/M3.)

Everything else is `torqcli` (`torq` is the same program), driven like a network device
(ADR-016, docs/CLI-DESIGN.md): edit a candidate, look at it, commit it as one ledgered change.
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
(e.g. SSH: keys only, only the owner, no root login; modules/services.nix). Always system changes,
so always confirmed:
```bash
torqcli show services                    # what the configuration turns on
torqcli -c "configure; set virtualisation podman enable true; commit"   # rootless containers
```
SSH is off on a new machine unless setup turned it on. To SSH in from the Mac, put your public key in
`~/.ssh/authorized_keys` in the VM (paste it in the UTM console), then `set services openssh enable
true` and `commit`: it shows exactly which keys will be able to log in, and refuses if there are
none (with keys only).
To move the machine to the newest TorqOS (and so nixpkgs) — one ledgered, undoable `flake.lock`
change: `request system update`, then `configure`, `show | compare`, `commit`.
`show system alarms` says whether the machine is as its ledger says; it is also checked at boot and
every 15 minutes, and a new shell warns if not. Every commit is checked against the whole system
first, then proposed and applied through the broker, so it is ledgered and can be undone.

Do not run `nixos-rebuild` against `/etc/nixos`: on TorqOS those files are stubs that refuse to
build (`torq.broker.guardEtcNixos`), because building them replaces the whole system outside
the ledger.

### 3. Editing from macOS
VS Code Remote-SSH (or run Claude Code inside the VM). Develop the distribution in `~/torq`;
the config repo picks up changes when its `torq` input is updated
(`nix flake update torq` in a scratch copy → diff → broker).

### 4. Local model on the Mac host (M2+)
Run llama.cpp (`llama-server`) or Ollama on macOS. From the VM, the Mac is usually the default
gateway (`ip route | grep default`). Set `torq.ai.modelEndpoint` accordingly.

## Throwaway VM (no install needed)
```bash
nix build .#nixosConfigurations.dev-vm-aarch64.config.system.build.vm
./result/bin/run-torq-dev-vm      # login: dev / torq
```
On macOS this needs a Linux builder (nix-darwin `nix.linux-builder.enable = true`); otherwise run
it inside the UTM VM or on a Linux server.

## Optional: x86_64 server
- Dedicated x86 box (Ryzen, 64 GB+), NixOS via nixos-anywhere.
- x86 builds, Secure Boot/TPM emulation tests (OVMF + swtpm), CI.
- Binary cache (Cachix or attic) so the Mac/VM downloads instead of building.
