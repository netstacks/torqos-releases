# Services are NixOS's own settings (`set services openssh enable true`, ADR-025): one place for each.
# What TorqOS adds is how they are set up safely, whichever way they are turned on — a person never
# has to know a second, safer name for the same service:
#   SSH (services.openssh): keys only unless passwords are allowed (`services openssh settings
#   PasswordAuthentication true`), then repeated wrong guesses are blocked (sshguard); 3 tries a
#   connection; only the owner may log in (AllowUsers, changeable); root never logs in remotely.
#   Podman: rootless; the root API socket (root-equivalent) stays off.
# Each is a default a person can change in the tree, except root's remote login, which TorqOS never
# allows (SECURITY-MODEL.md).
#
# Before this, TorqOS had a catalog of its own (`services.json`, a list such as [ "ssh" ], and
# `torq ssh passwords`). A configuration that still has them keeps working, mapped onto NixOS's
# settings, and is told how to move: the two settings, in one commit.
{ config, lib, ... }:

let
  ssh = config.services.openssh;
  owner = config.torq.user;
  dir = config.torq.packages.configDir;

  # The old catalog's names, and the NixOS setting each turned on.
  old = {
    ssh = [ "services" "openssh" "enable" ];
    podman = [ "virtualisation" "podman" "enable" ];
    tailscale = [ "services" "tailscale" "enable" ];
    printing = [ "services" "printing" "enable" ];
  };
  file = dir + "/services.json";
  oldList = if dir != null && builtins.pathExists file then lib.importJSON file else [ ];
  nixosPath = name: lib.concatStringsSep " " old.${name};
in
{
  options.torq.services = {
    enabled = lib.mkOption {
      type = lib.types.listOf (lib.types.enum (lib.attrNames old));
      default = [ ];
      description = "The old list of services (services.json): each is now NixOS's own setting, `services <name> enable true`. Read from services.json only.";
    };
    catalog = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      readOnly = true;
      default = lib.mapAttrs (name: path: "Now `${lib.concatStringsSep " " path} true`") old;
      description = "The old list's names, and the NixOS setting each is now (read-only).";
    };
  };

  options.torq.ssh.passwords = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = "The old name of `services openssh settings PasswordAuthentication`: SSH accepts passwords as well as keys.";
  };

  config = lib.mkMerge (
    [
      {
        torq.services.enabled = lib.mkIf (dir != null && builtins.pathExists file) oldList;

        torq.schema.services = {
          kind = "leaf-list";
          help = "The old list of services: each is now NixOS's own setting (`services openssh enable true`)";
          file = "services.json";
          values = config.torq.services.catalog;
        };

        assertions = [
          {
            # One source of truth for the old list: services.json, as torqcli shows it.
            assertion = config.torq.services.enabled == oldList;
            message = "torq.services.enabled is read from services.json only (the old list of services), not set in Nix.";
          }
          {
            assertion = !ssh.enable || ssh.settings.PermitRootLogin == "no";
            message = "TorqOS never lets root log in over SSH (services.openssh.settings.PermitRootLogin must be \"no\"): log in as a person, then use sudo.";
          }
        ];

        warnings =
          lib.optional (oldList != [ ])
            "services.json is TorqOS's old list of services; each is now NixOS's own setting. In torqcli: ${lib.concatMapStringsSep "; " (n: "`set ${nixosPath n} true`") oldList}; `delete torq services enabled`; then commit."
          ++ lib.optional config.torq.ssh.passwords
            "torq ssh passwords is the old name: in torqcli, `set services openssh settings PasswordAuthentication true`, `delete torq ssh passwords`, then commit.";
      }

      # SSH, safely, however it is turned on.
      (lib.mkIf ssh.enable {
        services.openssh.settings = {
          PasswordAuthentication = lib.mkDefault config.torq.ssh.passwords;
          KbdInteractiveAuthentication = lib.mkDefault false;
          MaxAuthTries = lib.mkDefault 3;
          AllowUsers = lib.mkIf (owner != null) (lib.mkDefault [ owner ]);
        };
        # Password guessing: an address that keeps failing is blocked for a while. sshguard reads the
        # journal by the name a program logs under; OpenSSH (9.8 on) logs logins, failed ones
        # included, as `sshd-session`, not `sshd`, which is all NixOS has it read (tests/ssh.nix).
        services.sshguard.enable = lib.mkDefault (ssh.settings.PasswordAuthentication == true);
        services.sshguard.services = lib.mkDefault [ "sshd" "sshd-session" ];
      })

      # Podman: rootless use needs no root API socket (group `podman`, root-equivalent).
      (lib.mkIf config.virtualisation.podman.enable {
        systemd.sockets.podman.wantedBy = lib.mkForce [ ];
      })
    ]
    # The old list, mapped onto NixOS's settings (one conditional definition each: which settings are
    # defined must not depend on a setting's value).
    ++ lib.mapAttrsToList (name: path: lib.mkIf (lib.elem name oldList) (lib.setAttrByPath path true)) old
  );
}
