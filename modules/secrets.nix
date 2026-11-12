# The secrets daemon (ADR-052): a program on this machine uses a secret in an HTTPS request, and never reads it.
# `torq-secrets` listens on 127.0.0.1:7800; each use is the broker's to judge (`secret-use`: who is connected,
# read from the kernel by the broker itself; the secret's rule; the agent's first use in each session on the
# record) and the value comes for that one request. The daemon is its own account, which can read no value and
# keeps none, in a unit that can do little but reach the network and the broker.
{ config, lib, pkgs, ... }:

let
  cfg = config.torq.secrets;
in
{
  options.torq.secrets = {
    enable = lib.mkOption {
      type = lib.types.bool;
      default = config.torq.broker.enable;
      defaultText = lib.literalExpression "config.torq.broker.enable";
      description = "The secrets daemon (ADR-052): secrets used in HTTPS requests by the programs their rules name.";
    };
    package = lib.mkOption {
      type = lib.types.package;
      description = "torq-secrets package (set by the TorqOS flake).";
    };
  };

  config = lib.mkIf cfg.enable {
    # A static system user: the broker names its caller from /etc/passwd (as it does the agent's).
    users.users.torq-secrets = {
      isSystemUser = true;
      group = "torq-secrets";
      description = "TorqOS secrets daemon (uses a secret, never keeps one)";
      hashedPassword = "!";
    };
    users.groups.torq-secrets = { };

    systemd.services.torq-secrets = {
      description = "TorqOS secrets daemon: secrets used, never read (ADR-052)";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" "torq-broker.socket" ];
      wants = [ "torq-broker.socket" ];
      # curl makes each request; its CA bundle is the system's (/etc/ssl/certs).
      # And OpenSSH's agent signs, a private one for each signature (secrets/src/agent.rs).
      path = [ pkgs.curl pkgs.openssh ];
      serviceConfig = {
        ExecStart = "${lib.getExe cfg.package} serve";
        User = "torq-secrets";
        Group = "torq-secrets";
        Restart = "on-failure";
        RestartSec = 2;
        # It holds a value for one request at a time: no core of it, ever (it is not dumpable either).
        LimitCORE = 0;
        # Little more than the network and the broker's socket.
        NoNewPrivileges = true;
        CapabilityBoundingSet = "";
        AmbientCapabilities = "";
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        ProtectProc = "default";
        RestrictAddressFamilies = [ "AF_INET" "AF_INET6" "AF_UNIX" ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [ "@system-service" "~@privileged" ];
        UMask = "0077";
        # The agent sockets (POST /ssh-agent): anyone may reach the folder; each socket answers one account.
        RuntimeDirectory = "torq-secrets";
        RuntimeDirectoryMode = "0755";
        # Bounded: a few requests at once, each at most 32 MiB each way.
        MemoryMax = "1G";
        TasksMax = 64;
      };
    };
  };
}
