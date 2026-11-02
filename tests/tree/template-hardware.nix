# A stand-in for the hardware-configuration.nix nixos-generate-config writes on a real machine,
# for evaluating the config repo template as a whole machine (tests/tree.nix, case "template").
{ ... }:
{
  fileSystems."/" = { device = "/dev/vda1"; fsType = "ext4"; };
  fileSystems."/boot" = { device = "/dev/vda2"; fsType = "vfat"; };
}
