{ config, lib, pkgs, ... }:

let
  cfg = config.torq.broker;
in
{
  options.torq.broker = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "The TorqOS change broker (sole writer of the config repo).";
    };
    package = lib.mkOption {
      type = lib.types.package;
      description = "torq-broker package (set by the TorqOS flake).";
    };
    configRepo = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/torq/config";
    };
    stateDir = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/torq/state";
    };
    configName = lib.mkOption {
      type = lib.types.str;
      default = "torq";
      description = ''
        The `nixosConfigurations.<name>` in the config repo that this machine is built from; the
        template's flake always exposes `torq`. The broker builds `<repo>#nixosConfigurations.<name>`
        explicitly, and the name is fixed rather than the host name: if it followed the host name,
        changing the host name would leave the next build looking for a configuration that does not
        exist.
      '';
    };
    rebuildCommand = lib.mkOption {
      type = lib.types.str;
      # What `nixos-rebuild switch` did, without it (ADR-032; the same as broker/src/config.rs
      # REBUILD): build the commit, make it the system profile, switch to it in a unit of its own.
      # --no-update-lock-file: a flake.lock that does not match flake.nix fails, never re-locked as root.
      default = lib.concatStrings [
        "d=$(mktemp -d) && trap 'rm -rf \"$d\"' EXIT"
        " && sys=$(nix build --no-update-lock-file --out-link \"$d/system\" --print-out-paths {flake}#nixosConfigurations.${cfg.configName}.config.system.build.toplevel)"
        " && nix-env -p /nix/var/nix/profiles/system --set \"$sys\""
        " && systemd-run --collect --no-ask-password --pipe --quiet --service-type=exec --unit=torq-switch --wait \"$sys/bin/switch-to-configuration\" switch"
      ];
      defaultText = lib.literalMD "build the commit, `nix-env --set` the system profile, `switch-to-configuration switch` in the transient unit `torq-switch` (broker/src/config.rs `REBUILD`)";
      description = ''
        What the broker runs after each commit: `{flake}` is the config repo at the commit just made
        (`git+file://…?rev=…`: that commit, never the working tree), `{repo}` its path.
        Part of the broker's fixed settings (/etc/torq/broker.json): a root broker refuses any other
        command from its caller. Tests set it to `true`.
      '';
    };
    # `commit confirmed` (ADR-016). Fixed settings too; overridden only by tests. See
    # broker/src/config.rs for what each does and its placeholders.
    buildCommand = lib.mkOption {
      type = lib.types.str;
      default = "nix build --no-update-lock-file --out-link {link} --print-out-paths {flake}#nixosConfigurations.${cfg.configName}.config.system.build.toplevel";
      defaultText = lib.literalExpression
        ''"nix build --no-update-lock-file --out-link {link} --print-out-paths {flake}#nixosConfigurations.''${config.torq.broker.configName}.config.system.build.toplevel"'';
      description = ''
        Build a commit confirmed's system without activating it; prints its store path. `{link}` is
        a GC root the broker removes once the change settles.
      '';
    };
    activateCommand = lib.mkOption {
      type = lib.types.str;
      default = "{system}/bin/switch-to-configuration test";
      description = "Activate a built system without making it the boot default: a commit confirmed, and its rollback.";
    };
    bootCommand = lib.mkOption {
      type = lib.types.str;
      default = "nix-env -p {profile} --set {system} && {system}/bin/switch-to-configuration boot";
      description = "Make a system the boot default: `confirm`.";
    };
    scheduleCommand = lib.mkOption {
      type = lib.types.str;
      # A wall-clock timer (it fires on resume if the deadline passed during suspend), retried a
      # bounded number of times while the broker is busy or the restore has not taken.
      default = lib.concatStringsSep " " [
        "systemd-run --quiet --collect --unit=torq-confirm-{seq} --on-calendar={deadline}"
        "--timer-property=AccuracySec=1s --property=Restart=on-failure --property=RestartSec=15"
        "--property=StartLimitIntervalSec=1200 --property=StartLimitBurst=60"
        "{broker} expire-confirm {seq}"
      ];
      description = "Schedule the rollback of an unconfirmed commit (a transient timer).";
    };
    guardEtcNixos = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Replace /etc/nixos/configuration.nix and /etc/nixos/flake.nix with stubs that refuse to
        build. A build of it by hand (`nix build`, or `nixos-rebuild` brought along) would otherwise build them and replace the whole
        system with a stock NixOS one, outside the broker and the ledger. Files already there
        (or symlinks) are moved aside to `<name>.pre-torq` first, never overwritten. Turning
        this off, or rolling back past it, deletes whatever is at those two paths (NixOS cleans up
        the files it installed) and does not move the old files back: while the guard is on, keep
        nothing of your own there.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];

    systemd.tmpfiles.rules = [
      "d /var/lib/torq 0755 root root -"
      "d ${cfg.stateDir} 0700 root root -"
      "d /run/torq 0755 root root -"
    ];

    # Drift from the ledger (the machine switched to another configuration, the config repo
    # changed behind the broker's back, an interrupted operation, a broken ledger) must not go
    # unnoticed: check at boot and every 15 minutes; problems go to /run/torq/drift (shown by
    # every new interactive shell) and to the journal at warning priority.
    # The unit never fails: it can run *during* a switch (the timer fires then), and a failed unit
    # fails the switch, which the broker treats as a failed rebuild and rolls back (ADR-008).
    # A monitor must not be able to undo the changes it watches.
    systemd.services.torq-status = {
      description = "Check that this machine is as its TorqOS ledger says";
      after = [ "torq-broker-init.service" ];
      wantedBy = [ "multi-user.target" ];
      restartIfChanged = false;
      serviceConfig.Type = "oneshot";
      # Never exits non-zero (see above), whatever happens in here.
      script = ''
        set +e
        out=$(${lib.getExe cfg.package} status 2>&1)
        code=$?
        mkdir -p /run/torq
        case $code in
          0)
            rm -f /run/torq/drift
            printf '%s\n' "$out" ;;
          75)  # an operation is running, nothing compared: keep any earlier finding as it is
            printf '%s\n' "$out" ;;
          *)
            { printf '%s\n' "$out" | grep '^PROBLEM' || printf 'PROBLEM: %s\n' "$out"; } > /run/torq/drift.new
            mv -f /run/torq/drift.new /run/torq/drift  # atomic: a shell never sees half of it
            printf '%s\n' "$out" | sed 's/^/<4>/' ;;  # journal priority: warning
        esac
        exit 0
      '';
    };
    systemd.timers.torq-status = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "2min";
        OnUnitActiveSec = "15min";
      };
    };
    environment.interactiveShellInit = ''
      if [ -s /run/torq/drift ]; then
        echo "TorqOS: this machine is not as its ledger says (details: torqcli show system alarms):" >&2
        sed 's/^PROBLEM: /  - /' /run/torq/drift >&2
      fi
    '';

    # The broker's fixed settings: a root broker takes them only from here and refuses a caller's
    # flags or TORQ_* variables that differ (broker/src/config.rs; KNOWN-GAPS #4). PATH and HOME for
    # what it runs are fixed too, never the caller's.
    environment.etc = lib.mkMerge [
     {
      "torq/broker.json".text = builtins.toJSON {
        repo = cfg.configRepo;
        stateDir = cfg.stateDir;
        rebuild = cfg.rebuildCommand;
        build = cfg.buildCommand;
        activate = cfg.activateCommand;
        boot = cfg.bootCommand;
        schedule = cfg.scheduleCommand;
        currentSystem = "/run/current-system";
        systemProfile = "/nix/var/nix/profiles/system";
        # The tools undo depends on, by store path first: a reviewed but mistaken change that drops
        # git or nix from the system must not leave `revert` unable to run them. The
        # running system's tools come after, for anything else.
        path = lib.makeBinPath [
          config.nix.package
          pkgs.gitMinimal
          pkgs.coreutils
          pkgs.bash
          config.systemd.package
        ] + ":/run/current-system/sw/bin";
        home = "/root";
        # The only accounts whose password the broker sets (modules/accounts.nix).
        people = config.torq.accounts.people;
      };
     }

    # A stray build of /etc/nixos (a person, or another tool following stock NixOS habits) must
    # fail loudly instead of switching the machine away from its ledgered config.
     (lib.mkIf cfg.guardEtcNixos (
      let
        # Deliberately does not name the moved-aside files: building any other config is the same
        # mistake.
        message = ''
          This machine is managed by TorqOS. Its configuration is ${cfg.configRepo}, and only
          torq-broker changes it, so that every change is ledgered and can be undone:
            torqcli                     (configure; set …; commit — software, services, settings)
            sudo torq-broker propose --diff change.diff --reason "..."   (anything else)
          Do not build /etc/nixos or any other configuration yourself: it would replace
          this whole system with a stock NixOS one, outside the ledger.
        '';
        # Real files, not the usual symlinks into /etc/static: nix copies a flake's directory to
        # the store, where an absolute symlink no longer resolves, and nixos-rebuild follows a
        # symlinked flake.nix to its own directory. Either way the message would never be seen.
        stub = text: { inherit text; mode = "0444"; };
      in
      {
        "nixos/configuration.nix" = stub ''
          # Managed by TorqOS (torq.broker.guardEtcNixos). See the message below.
          throw '''
          ${message}'''
        '';
        "nixos/flake.nix" = stub ''
          # Managed by TorqOS (torq.broker.guardEtcNixos). See the message below.
          {
            outputs = _: throw '''
          ${message}''';
          }
        '';
      }
    ))
    ];
    # /etc activation replaces existing files silently: move the owner's files aside first.
    # A symlink is moved as a link, never opened (it could point at a FIFO or a hung mount, and this
    # runs at boot). A regular file is left in place only if it is byte-identical to our stub in the
    # running or the new generation; anything else — even a stub someone edited — is moved aside.
    system.activationScripts.torq-guard-etc-nixos = lib.mkIf cfg.guardEtcNixos ''
      for f in configuration.nix flake.nix; do
        p=/etc/nixos/$f
        [ -e "$p" ] || [ -L "$p" ] || continue
        if [ -f "$p" ] && [ ! -L "$p" ]; then
          for stub in "/etc/static/nixos/$f" "$systemConfig/etc/nixos/$f"; do
            [ -f "$stub" ] && ${pkgs.diffutils}/bin/cmp -s "$p" "$stub" && continue 2
          done
        fi
        keep=$p.pre-torq
        { [ -e "$keep" ] || [ -L "$keep" ]; } && keep="$keep.$(date +%Y%m%d-%H%M%S)"
        mv "$p" "$keep"
      done
    '';
    system.activationScripts.etc.deps = lib.mkIf cfg.guardEtcNixos [ "torq-guard-etc-nixos" ];
    assertions = lib.mkIf cfg.guardEtcNixos [{
      # With a mutable /etc overlay, moving the old file aside leaves a whiteout that hides the stub.
      assertion = !(config.system.etc.overlay.enable or false);
      message = "torq.broker.guardEtcNixos does not support system.etc.overlay yet.";
    }];

    # Initialise repo + ledger once a config flake has been placed in the repo directory.
    # Runs exactly once: after that, only the broker writes to the repo. Re-running it on every
    # boot or switch would commit out-of-band edits as "baseline" with no ledger entry, and
    # during a broker apply it collides with the broker lock and fails the switch.
    # Or: a first-boot setup cut short after its switch (its record, setup.json) — this system's
    # first start finishes it (the broker's `init`; while setup itself still runs, it waits for
    # nothing and changes nothing). The `|` conditions: either one, with flake.nix.
    systemd.services.torq-broker-init = {
      description = "Initialise TorqOS config repo and ledger";
      wantedBy = [ "multi-user.target" ];
      restartIfChanged = false;
      unitConfig.ConditionPathExists = [
        "${cfg.configRepo}/flake.nix"
        "|!${cfg.configRepo}/.git"
        "|${cfg.stateDir}/setup.json"
      ];
      # Its settings (and PATH) come from /etc/torq/broker.json.
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${lib.getExe cfg.package} init";
        # TODO(M3): run the broker as a socket-activated daemon with tighter sandboxing.
        ProtectHome = true;
        PrivateTmp = true;
        NoNewPrivileges = true;
      };
    };

    # A commit confirmed still waiting at boot was not confirmed before the machine restarted: its
    # timer was transient and is gone, and the boot default was never changed, so this boot already
    # runs the system before it. Put the config back to match and ledger the rollback. Nothing
    # waiting: nothing to do. A switch that first adds this unit starts it too; the broker only rolls
    # back if the boot ID differs from the one recorded with the change, i.e. after a real restart.
    systemd.services.torq-confirm-boot = {
      description = "Roll back a TorqOS commit confirmed that was not confirmed before a restart";
      after = [ "torq-broker-init.service" ];
      before = [ "torq-status.service" ];
      wantedBy = [ "multi-user.target" ];
      restartIfChanged = false;
      startLimitIntervalSec = 600;
      startLimitBurst = 30; # then failed, and `torq-broker status` reports it overdue
      serviceConfig = {
        # Not oneshot: boot (and login) must not wait for it. The boot default is already the old
        # system, so nothing waits on the rollback being ledgered.
        Type = "exec";
        RemainAfterExit = true;
        ExecStart = "${lib.getExe cfg.package} expire-confirm --reboot";
        Restart = "on-failure"; # busy: try again
        RestartSec = 5;
      };
    };
  };
}
