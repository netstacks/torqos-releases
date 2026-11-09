# The TorqOS image's own system (docs/INSTALL.md): a complete TorqOS with no owner yet. The console
# runs first-boot setup instead of a login prompt; nobody can log in, nothing listens.
#   nix build .#image   (on aarch64-linux: a qcow2 for UTM, QEMU, Proxmox)
{ ... }:
{
  imports = [ ./hardware-configuration.nix ./disk.nix ];
  torq.setup.enable = true;
  networking.hostName = "torq";
  # Setup joins Wi-Fi on this, the image's own boot (ADR-048, amended: network first): iwd, as an installed
  # machine with Wi-Fi has (cli/src/install.rs `found`). Its saved networks are this disk's, so the installed
  # system rejoins by itself. A few MB.
  networking.wireless.iwd.enable = true;
  boot.loader.systemd-boot.enable = true;
  boot.loader.efi.canTouchEfiVariables = true;
}
