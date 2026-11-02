# The TorqOS image's own system (docs/INSTALL.md): a complete TorqOS with no owner yet. The console
# runs first-boot setup instead of a login prompt; nobody can log in, nothing listens.
#   nix build .#image   (on aarch64-linux: a qcow2 for UTM, QEMU, Proxmox)
{ ... }:
{
  imports = [ ./hardware-configuration.nix ];
  torq.setup.enable = true;
  networking.hostName = "torq";
  boot.loader.systemd-boot.enable = true;
  boot.loader.efi.canTouchEfiVariables = true;
}
