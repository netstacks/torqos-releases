# The TorqOS installer's own system (ADR-031, docs/INSTALL.md): an ISO to boot from a USB stick or
# disc, whose console puts TorqOS on one of the machine's disks (modules/installer.nix).
#   nix build .#iso
{ lib, pkgs, modulesPath, ... }:
{
  imports = [ (modulesPath + "/installer/cd-dvd/iso-image.nix") ];
  torq.installer.enable = true;
  networking.hostName = "torq-installer";

  # UEFI only, as the image it installs: an installer started the old way (BIOS) would put on the
  # disk a system that machine cannot start.
  isoImage.makeBiosBootable = false;
  isoImage.makeEfiBootable = true;
  isoImage.makeUsbBootable = true;
  isoImage.volumeID = "TORQOS";
  isoImage.appendToMenuLabel = " installer";
  # torqos-x86_64-linux.iso, beside the image's torqos-x86_64-linux.qcow2.
  image.baseName = lib.mkForce "torqos-${pkgs.stdenv.hostPlatform.system}";

  # Whatever machine it starts on: the drivers and firmware of every disk, controller and screen
  # it may need — NixOS's own installer's choice. The machine it installs carries only its own
  # (the detected hardware file).
  hardware.enableAllHardware = true;
}
