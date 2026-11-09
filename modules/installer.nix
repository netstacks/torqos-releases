# The ISO installer (ADR-031, docs/INSTALL.md): a TorqOS live system whose console runs `torq
# install` and nothing else. It carries the TorqOS image — the disk CI builds and tests — and writes
# it to the disk a person chooses, with this machine's hardware as NixOS detects it, for first-boot
# setup on the new disk. Nobody can log in to it, nothing listens, and it keeps nothing: it is the
# image's way onto real hardware, not a machine of its own.
{ config, lib, pkgs, ... }:

let
  cfg = config.torq.installer;
in
{
  options.torq.installer = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "This is the TorqOS installer (the ISO): its console installs TorqOS on a disk, and there is no account to log in to.";
    };
    payload = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "The TorqOS image as the installer carries it (`image.raw.zst`, its `size` and `sha256`), set by the flake.";
    };
    firstBuild = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = ''
        What a first boot builds from (ADR-048 5, hosts/installer/first-build.nix), set by the flake: carried in
        the ISO's store, and copied by `torq install` onto the disk it installs, so first-boot setup builds
        change #1 with no network.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      { assertion = cfg.payload != null; message = "torq.installer needs torq.installer.payload (the flake sets it)."; }
      { assertion = !config.torq.setup.enable; message = "torq.installer is not the image: setup runs on the disk it installs, not on the installer."; }
      { assertion = config.torq.user == null; message = "torq.installer has no owner: nobody logs in to it."; }
    ];

    # What `torq install` writes and checks (cli/src/install.rs), and the image's disk for the hardware
    # file it leaves (the same file systems as hosts/image/hardware-configuration.nix: checked by
    # tests/installer.nix, which boots the installed disk).
    environment.etc = {
      "torq/installer/image.raw.zst".source = "${cfg.payload}/image.raw.zst";
      "torq/installer/size".source = "${cfg.payload}/size";
      "torq/installer/sha256".source = "${cfg.payload}/sha256";
      "torq/installer/file-systems.nix".source = ../hosts/installer/file-systems.nix;
      # The installer's own medium, by its label: never offered, even copied to RAM and unmounted.
      "torq/installer/volume".text = config.isoImage.volumeID;
      # The first build's store paths, and their registration for the new disk's Nix database
      # (`store-paths`, `registration`): `torq install` copies what the disk lacks, setup registers it.
      "torq/installer/first-build".source = pkgs.closureInfo { rootPaths = cfg.firstBuild; };
    };

    # Nothing on the machine's disks is started: an LVM volume group or RAID array an earlier
    # system left would hold its disks, which could then not be chosen. And no network: the
    # installer needs none, so nothing reaches it.
    services.lvm.enable = lib.mkForce false;
    boot.swraid.enable = lib.mkForce false;
    networking.useDHCP = lib.mkForce false;
    networking.dhcpcd.enable = lib.mkForce false;

    # Detecting this machine's hardware is NixOS's own tool, which installed machines leave out
    # (modules/base.nix): on the installer only.
    system.tools.nixos-generate-config.enable = true;
    # The image is compressed with zstd; blkid, blockdev and mount are util-linux's.
    environment.systemPackages = [ pkgs.zstd pkgs.util-linux ];

    # No configuration to keep, no change to make: no broker, no history, no checks against one.
    torq.broker.enable = false;

    # Nobody can log in: no account, and no emergency shell if the boot fails (NixOS's ISO turns one
    # on with a systemd initrd).
    users.allowNoPasswordLogin = true;
    boot.initrd.systemd.emergencyAccess = lib.mkForce false;

    # No key stops the machine mid-write: Ctrl-Alt-Del starts nothing (its target masked), and
    # pressed seven times in two seconds, nothing either (systemd's burst action is a forced reboot);
    # the power, reboot, suspend and hibernate keys and the lid do nothing (logind would stop the
    # installer, which then cannot clear a disk it has not finished). What else can stop a write —
    # a power cut, a reset button, the power key held down — leaves no partition table: the
    # installer writes the image's last (cli/src/install.rs `put`).
    systemd.ctrlAltDelUnit = "/dev/null";
    systemd.settings.Manager.CtrlAltDelBurstAction = "none";
    services.logind.settings.Login = {
      HandlePowerKey = "ignore";
      HandlePowerKeyLongPress = "ignore";
      HandleRebootKey = "ignore";
      HandleRebootKeyLongPress = "ignore";
      HandleSuspendKey = "ignore";
      HandleSuspendKeyLongPress = "ignore";
      HandleHibernateKey = "ignore";
      HandleHibernateKeyLongPress = "ignore";
      HandleLidSwitch = "ignore";
      HandleLidSwitchExternalPower = "ignore";
      HandleLidSwitchDocked = "ignore";
    };

    systemd.services.torq-install = {
      description = "TorqOS installer";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-user-sessions.service" ];
      conflicts = [ "getty@tty1.service" ];
      restartIfChanged = false;
      serviceConfig = {
        # The devices found at boot set up first, as setup does (modules/setup.nix): a graphics
        # driver loading late takes the console over and loses what was on it; and the disks to
        # offer are all there. A device that never settles delays the installer a minute (`-`).
        ExecStartPre = "-${config.systemd.package}/bin/udevadm settle --timeout=60";
        Type = "idle";
        ExecStart = "${lib.getExe config.torq.cli.package} install";
        StandardInput = "tty";
        StandardOutput = "tty";
        StandardError = "tty";
        TTYPath = "/dev/tty1";
        TTYReset = true;
        TTYVHangup = true;
        Restart = "on-failure";
      };
    };
  };
}
