# Throwaway VM for fast iteration:
#   nix build .#nixosConfigurations.dev-vm-aarch64.config.system.build.vm
#   ./result/bin/run-torq-dev-vm
# Login: dev / torq
{ lib, ... }:
{
  networking.hostName = "torq-dev";
  torq.user = "dev";
  # A throwaway VM's fixed password, instead of a password file (modules/accounts.nix).
  users.users.dev = { initialPassword = "torq"; hashedPasswordFile = lib.mkForce null; };

  services.openssh.enable = true;

  # Needed for the non-VM toplevel to evaluate; the VM variant overrides these.
  fileSystems."/" = { device = "/dev/disk/by-label/nixos"; fsType = "ext4"; };
  boot.loader.systemd-boot.enable = true;

  virtualisation.vmVariant.virtualisation = {
    memorySize = 4096;
    cores = 4;
    graphics = true;
  };
}
