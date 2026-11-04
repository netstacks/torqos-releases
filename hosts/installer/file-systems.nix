# The TorqOS image's disk (hosts/image/hardware-configuration.nix, its file systems and growing),
# as the hardware file the installer leaves for a machine installed from the ISO names it, beside
# that machine's detected hardware (ADR-031, cli/src/install.rs).
{ ... }:
{
  fileSystems."/" = {
    device = "/dev/disk/by-label/nixos";
    autoResize = true;
    fsType = "ext4";
  };
  fileSystems."/boot" = {
    device = "/dev/disk/by-label/ESP";
    fsType = "vfat";
  };
  boot.growPartition = true;
}
