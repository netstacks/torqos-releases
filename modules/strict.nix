# Strict mode (ADR-035): root rare, and loud. An administrator's sudo runs two things, break-glass and
# the agent's launcher, and not the broker: torqcli needs no sudo (ADR-040), and the broker's commands
# under sudo are root's (it reads any file named to it, sets passwords). No `sudo bash`, and
# `torq break-glass "<reason>"` is the one way to a root shell: its
# entry in the ledger first, off the machine if there is a remote, and only then the shell. Off by
# default: a choice, in the tree (`torq security strict enable true`, a system change). The agent has
# no sudo at all.
{ config, lib, pkgs, ... }:

let
  cfg = config.torq.security.strict;
  broker = lib.getExe config.torq.broker.package;
  # Membership is root by another name (a container with / mounted, the raw disk): the group is
  # ledgered, each use would not be.
  rootGroups = [ "docker" "podman" "lxd" "incus-admin" "libvirtd" "disk" ];
  inRootGroup = lib.unique (
    lib.attrNames (lib.filterAttrs (_: u: lib.intersectLists rootGroups ([ u.group ] ++ u.extraGroups) != [ ]) config.users.users)
    ++ lib.concatMap (g: config.users.groups.${g}.members or [ ]) rootGroups
  );
  # As root, through sudo (the person's password): the record, then the shell.
  breakGlass = pkgs.writeShellApplication {
    name = "torq-break-glass";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      [ "$(id -u)" = 0 ] || { echo "torq-break-glass: run it with torq break-glass" >&2; exit 1; }
      by=''${SUDO_USER:?torq-break-glass: run it with torq break-glass}
      [ $# -gt 0 ] || { echo "torq-break-glass: say why" >&2; exit 1; }
      reason=$*
      code=0
      ${broker} break-glass --by "$by" --reason="$reason" || code=$?
      case "$code" in
        0) ;;
        3)
          echo "The entry is in this machine's ledger, not yet off it: the next push takes it, within 15 minutes." >&2
          read -r -p "Open the root shell all the same? (yes / no) " go
          [ "$go" = yes ] || exit 1
          ;;
        *) echo "Not on the record: no shell." >&2; exit 1 ;;
      esac
      echo "Root shell, on the record. Everything here goes around the broker: exit when you are done." >&2
      exec /run/current-system/sw/bin/bash --login
    '';
  };
in
{
  options.torq.security.strict.enable = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = ''
      An administrator's sudo runs break-glass and the agent's launcher, nothing else (torqcli needs no sudo);
      a root shell is `torq break-glass "<reason>"`, which writes its ledger entry first. Not with
      dev mode, which is passwordless sudo.
    '';
  };

  config = lib.mkMerge [
    # break-glass is there whether or not sudo is limited: without strict mode it is the shell with
    # a record, for those who want one.
    (lib.mkIf config.torq.broker.enable { environment.systemPackages = [ breakGlass ]; })

    (lib.mkIf cfg.enable {
      assertions = [
      {
        assertion = inRootGroup == [ ];
        message = "torq.security.strict: ${lib.concatStringsSep ", " inRootGroup} would have root without break-glass (a member of ${lib.concatStringsSep ", " rootGroups} needs no sudo): take them out of it first.";
      }
      {
        assertion = !config.torq.dev.enable;
        message = "torq.security.strict: not with dev mode (torq.dev.enable), which is passwordless sudo for building TorqOS.";
      }
      ];
      # Not by another door: polkit, wherever it comes on, takes root alone for an administrator.
      security.polkit.adminIdentities = [ "unix-user:0" ];
      # Its password every time: a shell is never opened on the last command's.
      security.sudo.extraConfig = "Defaults!${breakGlass}/bin/torq-break-glass timestamp_timeout=0\n";
      security.sudo.extraRules = lib.mkForce [
        { users = [ "root" ]; commands = [{ command = "ALL"; options = [ "SETENV" ]; }]; }
        {
          groups = [ "wheel" ];
          runAs = "root";
          commands = [
            { command = "${breakGlass}/bin/torq-break-glass"; options = [ ]; }
            { command = "${config.torq.agent.launcher}/bin/torq-agent-run"; options = [ "NOPASSWD" ]; }
          ];
        }
      ];
    })
  ];
}
