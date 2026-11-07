# The TorqOS image's disk (hosts/image/hardware-configuration.nix, its file systems and growing),
# as the hardware file the installer leaves for a machine installed from the ISO names it, beside
# that machine's detected hardware (ADR-031, cli/src/install.rs). One btrfs file system (ADR-038):
# `/home` and `/var/lib` are snapshotted; `/var/lib/torq`, the ledger, the configuration and the password
# hashes, is a subvolume of its own, so no snapshot of `/var/lib` holds it: restoring data must never
# rewind the record.
{ ... }:
let
  subvolume = name: {
    device = "/dev/disk/by-label/nixos";
    fsType = "btrfs";
    options = [ "subvol=${name}" ];
  };
in
{
  fileSystems."/" = subvolume "@" // { autoResize = true; };
  # Activation reads and writes them: new accounts' homes, the users' database, the password hashes.
  fileSystems."/home" = subvolume "@home" // { neededForBoot = true; };
  fileSystems."/var/lib" = subvolume "@var-lib" // { neededForBoot = true; };
  # A file system of its own, root's alone: the record has room whoever fills the rest (the audit of M3,
  # M2), and no snapshot of /var/lib holds it.
  fileSystems."/var/lib/torq" = { device = "/dev/disk/by-label/torq"; fsType = "ext4"; neededForBoot = true; };
  fileSystems."/.snapshots" = subvolume "@snapshots";
  fileSystems."/boot" = {
    device = "/dev/disk/by-label/ESP";
    fsType = "vfat";
  };
  boot.growPartition = true;
}
