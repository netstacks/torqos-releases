# First-boot setup (docs/INSTALL.md): on the image, before the machine has a configuration, the
# console asks for one (`torqcli setup`) instead of offering a login — host name, time zone, the
# owner's account and password, how to log in remotely — writes the config repo and hands it to the
# broker. Once the repo exists this never runs again, and the machine it switches to is built from
# the repo, where setup is off.
{ config, lib, ... }:

let
  cfg = config.torq.setup;
  repo = config.torq.broker.configRepo;
  # The image's own TorqOS, pinned in the new config repo: its source in the store, locked by hash,
  # and the nixpkgs TorqOS itself is locked to — so evaluating the machine needs no network.
  src = cfg.source;
  # Every input TorqOS's flake takes, locked as TorqOS locks it (nixpkgs, nixpkgs-cosmic since ADR-045's
  # amendment): one missing and the flake cannot even be called ("called without required argument").
  torqLock = lib.importJSON "${src.outPath}/flake.lock";
  torqInputs = torqLock.nodes.${torqLock.root}.inputs;
  lock = assert lib.all builtins.isString (lib.attrValues torqInputs); builtins.toJSON {
    version = 7;
    root = "root";
    nodes = {
      root.inputs = { torq = "torq"; nixpkgs = [ "torq" "nixpkgs" ]; };
      torq = {
        inputs = torqInputs;
        locked = { type = "path"; path = "${src.outPath}"; narHash = src.narHash; lastModified = src.lastModified or 0; };
        original = { type = "path"; path = "${src.outPath}"; };
      };
    } // lib.genAttrs (lib.attrValues torqInputs) (n: torqLock.nodes.${n});
  };
in
{
  options.torq.setup = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "This is the TorqOS image, before first-boot setup: the console runs setup, and there is no account to log in to.";
    };
    source = lib.mkOption {
      type = lib.types.nullOr lib.types.attrs;
      default = null;
      description = "The image's own TorqOS source (the flake's sourceInfo: outPath, narHash, lastModified), set by the flake.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      { assertion = config.torq.user == null; message = "torq.setup is the image's, before an owner exists."; }
      { assertion = src != null; message = "torq.setup needs torq.setup.source (the flake sets it)."; }
    ];

    # Nobody can log in to the image, deliberately: setup creates the first account (NixOS would
    # otherwise refuse a system without one).
    users.allowNoPasswordLogin = true;

    # What setup writes the config repo from: TorqOS's template, the lock above, the image's disk.
    environment.etc = {
      "torq/image/source".source = src.outPath;
      "torq/image/flake.lock".text = lock;
      "torq/image/hardware-configuration.nix".source = ../hosts/image/hardware-configuration.nix;
    };
    # Evaluating the new machine reads both its sources, TorqOS and nixpkgs: in the image, not
    # fetched (every system keeps them: lib.mkSystem, flake.nix).

    # Setup is the only way in: the image never takes over a repo by itself (`init` would ledger
    # files an interrupted setup left, unbuilt, as the machine's, with no account to log in to).
    systemd.services.torq-broker-init.enable = false;
    # Nor checks it against its history: before change #1 there is none, and a finding then would
    # outlive setup (a check during setup's switch finds the broker busy and keeps it), so the
    # prompt would say "not as its history says" on a machine that is. The set-up system checks.
    systemd.services.torq-status.enable = false;
    systemd.timers.torq-status.enable = false;

    systemd.services.torq-setup = {
      description = "TorqOS first-boot setup";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-user-sessions.service" ];
      conflicts = [ "getty@tty1.service" ];
      # Setup runs the broker, which switches to the new system — where this unit does not exist. A
      # switch stops units it removes, which would kill setup and the broker mid-switch: this one
      # finishes instead, and exits by itself. Set up means the history has its first entry: a setup
      # cut short before that — its files written, its build begun, or not — runs again at the next
      # boot, and starts clean.
      unitConfig = {
        ConditionFileNotEmpty = "!${config.torq.broker.stateDir}/ledger.jsonl";
        X-StopOnRemoval = false;
      };
      restartIfChanged = false;
      serviceConfig = {
        Type = "idle";
        ExecStart = "${lib.getExe config.torq.cli.package} setup";
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
