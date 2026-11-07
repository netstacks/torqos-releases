{ config, lib, pkgs, ... }:

let
  cfg = config.torq.security;
  fence = import ./settings-fence.nix { inherit lib; };
  # Wrong passwords, counted where only root can reach (a person can reset a count in its own file,
  # in faillock's default directory): the broker's check (modules/broker.nix) and these share it.
  faillockDir = "/run/torq/faillock";
  faillock = "${pkgs.pam}/lib/security/pam_faillock.so";
  limits = [ "dir=${faillockDir}" "deny=5" "unlock_time=600" ];
  # The same five-and-ten-minutes as the broker's, around pam_unix: preauth refuses a locked
  # account before its password is tried; a wrong one is counted (authfail), a right one clears the
  # count (authsucc). pam_unix itself skips authfail on success (it is no longer `sufficient`).
  lockout = {
    rules.auth = {
      faillock.enable = lib.mkForce false; # NixOS's own, for su: no arguments, and the default directory
      torq-faillock-preauth = { order = 11600; control = "required"; modulePath = faillock; args = [ "preauth" ] ++ limits; };
      unix.control = lib.mkForce "[success=1 default=ignore]";
      torq-faillock-authfail = { order = 11710; control = "[default=die]"; modulePath = faillock; args = [ "authfail" ] ++ limits; };
      torq-faillock-authsucc = { order = 11720; control = "sufficient"; modulePath = faillock; args = [ "authsucc" ] ++ limits; };
    };
    rules.account.torq-faillock = { order = 10900; control = "required"; modulePath = faillock; args = [ "dir=${faillockDir}" ]; };
  };
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

    passwordLockout.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Five wrong passwords at sudo or passwd lock the account there for ten minutes, as at the broker's own check (ADR-040): a program running as a person cannot guess their password without limit. Counted in ${faillockDir}, root's alone (`faillock --dir ${faillockDir} --user <name> --reset`, as root, to clear it).";
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

    (lib.mkIf cfg.passwordLockout.enable {
      systemd.tmpfiles.settings."10-torq-faillock".${faillockDir}.d = { mode = "0700"; user = "root"; group = "root"; };
      security.pam.services.sudo = lockout;
      # su is root's alone (below): it never asks a password, so it has none to count.
      security.pam.services.passwd = lockout;
    })

    # su is root's alone. Root's own password is locked, so su only ever let one person become another,
    # and every attempt spent the target's five wrong guesses: any account could lock the owner out of
    # sudo, break-glass and every system change, as often as it liked (the audit of M3, M3). A person
    # becomes another by being them; root still uses su as ever (pam_rootok, before this). Refused before
    # any password is asked, so nothing is guessed, and nothing counted.
    {
      security.pam.services.su.rules.auth.torq-root-only = {
        order = 10250;
        control = "requisite";
        modulePath = "${pkgs.pam}/lib/security/pam_deny.so";
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
