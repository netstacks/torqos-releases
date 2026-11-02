{ config, lib, ... }:

let
  cfg = config.torq.security;
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
      }];

      networking.firewall.enable = true;
      security.sudo.execWheelOnly = true;
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
