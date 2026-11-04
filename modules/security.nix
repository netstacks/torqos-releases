{ config, lib, ... }:

let
  cfg = config.torq.security;
  fence = import ./settings-fence.nix { inherit lib; };
  # Each /etc entry whose name or target climbs, is empty, or lands in torq/ under another name.
  etcAstray = lib.attrNames (lib.filterAttrs (name: e:
    let
      t = fence.etcSegments name e;
      n = fence.etcSegments name { };
      torq = t != [ ] && lib.head t == "torq";
    in
    builtins.elem ".." t || builtins.elem ".." n || t == [ ] || (torq && t != n)
  ) config.environment.etc);
in
{
  options.torq.security = {
    hardening.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Baseline kernel and userspace hardening.";
    };

    lockdown = lib.mkOption {
      type = lib.types.enum [ "none" "integrity" "confidentiality" ];
      default = "integrity";
      description = "Kernel lockdown mode.";
    };

    secureBoot.enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Owner-key Secure Boot via Lanzaboote (see docs/SECURITY-MODEL.md).
        Not implemented yet — TODO(M4). Off in VMs without emulated UEFI Secure Boot.
      '';
    };
  };

  config = lib.mkMerge [
    {
      assertions = [{
        assertion = !cfg.secureBoot.enable;
        message = "torq.security.secureBoot is not implemented yet (TODO(M4): wire up lanzaboote).";
      }
      # Where /etc's files land, as the evaluated system has them — whichever module made them
      # (environment.etc, or a service's own files: pppd's peers, an editor's runtime, …): none climbs
      # with `..`, none is the whole of /etc, and nothing lands in /etc/torq, the broker's, under
      # another name. The tree's settings are checked as written (settings-fence.nix); this is what
      # they become (audit of 2026-11-03: a pppd peer named `../../torq/broker.json` replaced it).
      {
        assertion = etcAstray == [ ];
        message = "torq: /etc files that land outside their place: ${lib.concatStringsSep ", " etcAstray} (a `..`, an empty target, or /etc/torq under another name)";
      }];

      networking.firewall.enable = true;
      security.sudo.execWheelOnly = true;
      # No editing the boot menu's entries: with it, anyone at the console adds `init=/bin/sh` to
      # one and starts a root shell, past the locked root account and the broker. (NixOS's default
      # is on.) A person who needs it turns it on, as a setting, ledgered.
      boot.loader.systemd-boot.editor = lib.mkDefault false;
      services.openssh.settings = {
        PermitRootLogin = "no";
      };
    }

    (lib.mkIf (cfg.lockdown != "none") {
      boot.kernelParams = [ "lockdown=${cfg.lockdown}" ];
    })

    (lib.mkIf cfg.hardening.enable {
      boot.kernel.sysctl = {
        "kernel.kptr_restrict" = 2;
        "kernel.dmesg_restrict" = 1;
        "kernel.unprivileged_bpf_disabled" = 1;
        "net.core.bpf_jit_harden" = 2;
        "kernel.yama.ptrace_scope" = 1;
        "fs.protected_symlinks" = 1;
        "fs.protected_hardlinks" = 1;
      };
    })
  ];
}
