{ config, lib, pkgs, ... }:

let
  cfg = config.torq.broker;
  # Read only, and root's alone: what a snapshot holds is every person's files as they were, and
  # keeping it from them is what lets it outlast what they, or a process of theirs, deletes.
  snapshot = pkgs.writeShellApplication {
    name = "torq-snapshot";
    runtimeInputs = [ pkgs.btrfs-progs pkgs.coreutils pkgs.findutils ];
    text = ''
      action=$1 label=$2
      case "$label" in "" | *[!0-9]*) echo "torq-snapshot: a label is digits" >&2; exit 2 ;; esac
      dir=/.snapshots/$label
      forget() {
        for s in "$1"/*; do
          [ -e "$s" ] && btrfs -q subvolume delete "$s"
        done
        rmdir "$1"
      }
      if [ "$action" = delete ]; then
        [ -d "$dir" ] || { echo "torq-snapshot: no snapshot $label" >&2; exit 1; }
        forget "$dir"
        exit 0
      fi
      # Room first: a disk too full for this one still gives up its oldest. Oldest first: labels are
      # times. One that will not go is said and stops nothing: this snapshot is what counts.
      find /.snapshots -mindepth 1 -maxdepth 1 -type d -name '[0-9]*' -printf '%f\n' | sort -n | head -n -${toString (config.torq.snapshots.keep - 1)} | while read -r old; do
        forget "/.snapshots/$old" || echo "torq-snapshot: $old would not go" >&2
      done
      mkdir -m 700 "$dir"
      if ! { btrfs -q subvolume snapshot -r /home "$dir/home" && btrfs -q subvolume snapshot -r /var/lib "$dir/var-lib"; }; then
        forget "$dir" || true
        exit 1
      fi
    '';
  };
in
{
  # The config repo's remote (ADR-036): where the configuration and its ledger also live, pushed after
  # every change. `torq remote url <ssh url>` in the tree (a system change); none by default (ADR-004).
  options.torq.remote.url = lib.mkOption {
    type = lib.types.nullOr (lib.types.strMatching "(ssh://([A-Za-z0-9._-]+@)?[A-Za-z0-9][A-Za-z0-9.-]*(:[0-9]+)?/[A-Za-z0-9._~/-]+|[A-Za-z0-9][A-Za-z0-9._-]*@[A-Za-z0-9][A-Za-z0-9.-]*:[A-Za-z0-9._~/][A-Za-z0-9._~/-]*)");
    default = null;
    example = "git@github.com:you/torq5-config.git";
    description = ''
      The config repo's remote, over SSH only (ssh://host/path, or user@host:path): after every change
      the broker pushes the configuration and its ledger there, never forced. A private repository,
      with branch protection (no force-push): then the ledger it holds cannot be taken back, and a
      ledger rewritten here is an alarm. The machine's key for it: `request system remote key`.
    '';
  };

  # Outside `torq.broker`, which the configuration tree may not reach: these are a person's to choose
  # (`set torq snapshots keep 20`), a system change, and never the agent's. What the broker runs for
  # them stays fenced.
  options.torq.snapshots = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = (config.fileSystems."/.snapshots".fsType or "") == "btrfs";
      defaultText = lib.literalExpression ''config.fileSystems."/.snapshots".fsType == "btrfs"'';
      description = ''
        Snapshot `/home` and `/var/lib` before each system change, read only, in `/.snapshots` (ADR-038). On by
        itself on a disk laid out for it, as the image's and the ISO's are; an ext4 machine has none.
      '';
    };
    keep = lib.mkOption {
      type = lib.types.ints.positive;
      default = 10;
      description = "How many to keep, newest first: what they hold stays on the disk, deleted or not, until they go. Only system changes take one, so only an administrator moves them on.";
    };
  };

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
    # faillock's counts of wrong passwords (ADR-040): root's alone, so a program running as a person
    # cannot reset its own between guesses. (The same entry as modules/security.nix's, for sudo and su.)
    systemd.tmpfiles.settings."10-torq-faillock"."/run/torq/faillock".d = { mode = "0700"; user = "root"; group = "root"; };

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
    # Daily: the running system's store paths against what the store recorded when each was built. A path
    # changed in place is one no rebuild looks at again (the audit of M3, M5); `status` says so, and
    # `repair` puts it back.
    systemd.services.torq-verify-store = {
      description = "Check the running system's store paths against the store's record of them";
      restartIfChanged = false;
      serviceConfig = { Type = "oneshot"; Nice = 19; IOSchedulingClass = "idle"; };
      script = "${lib.getExe cfg.package} verify-store > /dev/null || true";
    };
    systemd.timers.torq-verify-store = {
      wantedBy = [ "timers.target" ];
      timerConfig = { OnBootSec = "20min"; OnUnitActiveSec = "1d"; RandomizedDelaySec = "1h"; };
    };

    systemd.timers.torq-status = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "2min";
        OnUnitActiveSec = "15min";
      };
    };

    # With a remote (ADR-036): what a change could not push (the remote out of reach), pushed later.
    # Never fails: an unreachable remote is recorded for `show system remote`, not a broken unit.
    systemd.services.torq-remote-push = lib.mkIf (config.torq.remote.url != null) {
      description = "Push this machine's TorqOS configuration and ledger to its remote";
      after = [ "network-online.target" "torq-broker-init.service" ];
      wants = [ "network-online.target" ];
      restartIfChanged = false;
      serviceConfig.Type = "oneshot";
      script = "${lib.getExe cfg.package} push || true";
      # Each git call to the remote is stopped after a minute (the broker's); the whole, after five.
      serviceConfig.TimeoutStartSec = "5min";
    };
    systemd.timers.torq-remote-push = lib.mkIf (config.torq.remote.url != null) {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnBootSec = "3min";
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
        snapshot = lib.optionalString config.torq.snapshots.enable "${snapshot}/bin/torq-snapshot {action} {label}";
        currentSystem = "/run/current-system";
        systemProfile = "/nix/var/nix/profiles/system";
        # The tools undo depends on, by store path first: a reviewed but mistaken change that drops
        # git or nix from the system must not leave `revert` unable to run them. The
        # running system's tools come after, for anything else.
        path = lib.makeBinPath [
          config.nix.package
          pkgs.gitMinimal
          # The remote (ADR-036): git over SSH, and the machine's key for it.
          pkgs.openssh
          pkgs.coreutils
          pkgs.gnutar
          pkgs.util-linux
          pkgs.bash
          config.systemd.package
        ] + ":/run/current-system/sw/bin";
        home = "/root";
        # The only accounts whose password the broker sets (modules/accounts.nix).
        people = config.torq.accounts.people;
        # What the agent may propose (ADR-041, modules/agent.nix).
        agent = { inherit (config.torq.agent.mayPropose) users settings; };
        # The config repo's remote (ADR-036): none, or an SSH URL.
        remote = config.torq.remote.url;
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
            sudo torq-broker propose --diff change.diff --reason "..."   (anything else; not in strict mode)
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
    assertions = lib.mkMerge [ [{
      assertion = !config.torq.snapshots.enable || (lib.all (m: (config.fileSystems.${m}.fsType or "") == "btrfs") [ "/home" "/var/lib" "/.snapshots" ] && config.fileSystems ? "/var/lib/torq" && config.fileSystems."/var/lib/torq".device != config.fileSystems."/var/lib".device);
      message = "torq.snapshots: /home, /var/lib and /.snapshots must each be a btrfs subvolume, and /var/lib/torq a file system of its own: otherwise every snapshot of /var/lib would hold the ledger and the password hashes, and filling the disk would leave the record no room. Without that layout, set torq.snapshots.enable = false.";
    }] (lib.mkIf cfg.guardEtcNixos [{
      # With a mutable /etc overlay, moving the old file aside leaves a whiteout that hides the stub.
      assertion = !(config.system.etc.overlay.enable or false);
      message = "torq.broker.guardEtcNixos does not support system.etc.overlay yet.";
    }]) ];

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

    # The broker's door (ADR-040): a socket anyone may connect to, because who may do what is
    # decided inside — by the caller's uid, from the kernel (SO_PEERCRED), and the change's tier.
    # Each connection is one `torq-broker serve`, as root, its standard input and output the
    # connection. torqcli needs no sudo; `sudo torq-broker` stays the recovery door, but for strict
    # mode, where sudo does not run it (modules/strict.nix): there the door is break-glass.
    systemd.sockets.torq-broker = {
      description = "TorqOS change broker";
      wantedBy = [ "sockets.target" ];
      socketConfig = {
        ListenStream = "/run/torq/broker.sock";
        SocketMode = "0666";
        Accept = true;
        # A queue, not a crowd: the broker's lock serialises changes anyway.
        MaxConnections = 16;
        # Per account (a Unix socket counts its peers by uid): no one holds every connection.
        MaxConnectionsPerSource = 4;
      };
    };
    systemd.services."torq-broker@" = {
      description = "TorqOS change broker: one request";
      after = [ "torq-broker-init.service" ];
      # Never stopped or restarted by a switch — the switch it is running, often.
      restartIfChanged = false;
      stopIfChanged = false;
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} serve";
        StandardInput = "socket";
        StandardOutput = "socket";
        StandardError = "journal";
        # Its settings (and PATH) come from /etc/torq/broker.json, and it runs as it does under
        # sudo: no ProtectHome, since a development source (`path:/home/…`, ADR-009) is built from
        # a home.
      };
    };

    # A system change's password (ADR-040), checked by the broker as root: the account's own
    # password (pam_unix, never an empty one), and faillock counting failures, so that guessing
    # is slow, logged, and stops — five wrong, ten minutes locked.
    security.pam.services.torq-approve.text = ''
      auth     required       ${pkgs.pam}/lib/security/pam_faillock.so preauth dir=/run/torq/faillock deny=5 unlock_time=600
      auth     [success=1 default=ignore] ${pkgs.pam}/lib/security/pam_unix.so
      auth     [default=die]  ${pkgs.pam}/lib/security/pam_faillock.so authfail dir=/run/torq/faillock deny=5 unlock_time=600
      auth     sufficient     ${pkgs.pam}/lib/security/pam_faillock.so authsucc dir=/run/torq/faillock deny=5 unlock_time=600
      auth     required       ${pkgs.pam}/lib/security/pam_deny.so
      account  required       ${pkgs.pam}/lib/security/pam_faillock.so dir=/run/torq/faillock
      account  required       ${pkgs.pam}/lib/security/pam_unix.so
    '';

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
