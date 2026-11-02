{ config, lib, pkgs, ... }:

let
  cfg = config.torq;
in
{
  options.torq.user = lib.mkOption {
    # A login name's shape (as the broker's user tier takes them): never empty.
    type = lib.types.nullOr (lib.types.strMatching "[a-z_][a-z0-9_-]*");
    default = null;
    description = "Primary (owner) user account of this machine. None only in the image, before first-boot setup creates it (modules/setup.nix).";
  };

  config = {
    assertions = [
      {
        # Every configured machine has an owner; only the image, before setup, has none.
        assertion = cfg.user != null && cfg.user != "CHANGE-ME" || cfg.user == null && cfg.setup.enable;
        message = "Set the owner's login name: `torq user <name>` (nixos/torq.json in the config repo).";
      }
      {
        # The same shape the broker accepts for users/<name>/ (user tier).
        assertion = cfg.user == null || builtins.match "[a-z_][a-z0-9_-]*" cfg.user != null;
        message = "torq.user '${cfg.user}' must be a lowercase login name ([a-z_][a-z0-9_-]*).";
      }
    ];

    nix.settings = {
      experimental-features = [ "nix-command" "flakes" ];
      # A power cut mid-change must not leave the store broken: by default Nix records a path as
      # there before its files reach the disk, and a cut can leave it empty ("file is empty (possible
      # filesystem corruption)"), failing every later build. Found by cutting the power during
      # first-boot setup. Slower builds of many small paths; a machine that works after a cut.
      fsync-store-paths = true;
      # Only root (i.e. the broker via sudo/systemd) may drive system builds.
      trusted-users = [ "root" ];
      allowed-users = [ "root" "@wheel" ];
      auto-optimise-store = true;
      # During a build, collect garbage when less than 1 GiB is free, until 5 GiB is.
      min-free = lib.mkDefault (1024 * 1024 * 1024);
      max-free = lib.mkDefault (5 * 1024 * 1024 * 1024);
    };

    # The store must not fill the disk. Collecting old generations does not weaken undo: a broker
    # revert undoes the commit and rebuilds it (from the binary cache), it never needs an old
    # generation on disk (ADR-012). The running system is never collected.
    nix.gc = {
      automatic = lib.mkDefault true;
      dates = lib.mkDefault "weekly";
      options = lib.mkDefault "--delete-older-than 30d";
      persistent = lib.mkDefault true; # catch up after the machine was off
    };
    boot.loader.systemd-boot.configurationLimit = lib.mkDefault 20;

    users.users = lib.mkIf (cfg.user != null) {
      ${cfg.user} = {
        isNormalUser = true;
        extraGroups = [ "wheel" "video" "audio" ];
      };
    };

    # Thin (CLAUDE.md): the network as systemd does it, DHCP on every wired interface, rather than
    # NetworkManager, which brings Wi-Fi and mobile-modem support a server-like machine does not use
    # (~110 MB). The desktop, when it comes back, brings NetworkManager with it (desktop.nix).
    networking.useNetworkd = lib.mkDefault true;
    services.resolved.enable = lib.mkDefault true;
    time.timeZone = lib.mkDefault "America/New_York";
    i18n.defaultLocale = lib.mkDefault "en_US.UTF-8";

    # The tools a person at the shell expects. The editor is nano (NixOS's, small: `file edit` uses
    # it unless $EDITOR says otherwise); anything else is a package to install.
    environment.systemPackages = with pkgs; [ git curl htop jq ];
    # Nothing NixOS adds unasked (perl, rsync, strace on the shell's path); no manuals but the man
    # pages (the NixOS and Nix manuals, info, /share/doc: ~70 MB).
    environment.defaultPackages = lib.mkDefault [ ];
    documentation = {
      nixos.enable = lib.mkDefault false;
      info.enable = lib.mkDefault false;
      doc.enable = lib.mkDefault false;
    };

    system.stateVersion = lib.mkDefault "26.05";
  };
}
