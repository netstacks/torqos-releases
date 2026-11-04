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
        # Every configured machine has an owner; only the image, before setup, and the installer
        # (ADR-031), which installs the image, have none.
        assertion = cfg.user != null && cfg.user != "CHANGE-ME" || cfg.user == null && (cfg.setup.enable || cfg.installer.enable);
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
    # Names from the DNS servers only: resolved's link-local name answering (LLMNR, and multicast
    # DNS) listens on every interface, and lets anyone on the same network answer for a name — a
    # way to pass for another machine. Nothing listens on a TorqOS machine unless it is asked for
    # (tests/image.nix checks the image).
    services.resolved.settings.Resolve = {
      LLMNR = lib.mkDefault false;
      MulticastDNS = lib.mkDefault false;
    };
    time.timeZone = lib.mkDefault "America/New_York";
    i18n.defaultLocale = lib.mkDefault "en_US.UTF-8";

    # The skeleton (ADR-032): what TorqOS itself runs, and nothing a configuration can add. git is
    # the config repo's, which torqcli reads (the minimal build: no manuals, Perl or Python). The
    # editor is nano (NixOS's, small: `file edit` uses it unless $EDITOR says otherwise). Anything
    # else — htop, jq, a desktop — is a package or setting to put in the configuration.
    environment.systemPackages = [ pkgs.gitMinimal ];
    # Nothing NixOS adds unasked (perl, rsync, strace on the shell's path), and no documentation:
    # the NixOS and Nix manuals, info, /share/doc and the man pages with their tools (~85 MB). A
    # configuration turns it back on (`set documentation enable true`; the man pages need `set
    # documentation man enable true` as well).
    environment.defaultPackages = lib.mkDefault [ ];
    documentation.enable = lib.mkDefault false;
    documentation.man.enable = lib.mkDefault false;
    # Nothing a machine without a screen uses (as NixOS's own minimal profile): fonts, icons, sounds,
    # file types and autostart entries are a desktop's (desktop.nix turns them back on).
    fonts.fontconfig.enable = lib.mkDefault false;
    xdg = {
      autostart.enable = lib.mkDefault false;
      icons.enable = lib.mkDefault false;
      mime.enable = lib.mkDefault false;
      sounds.enable = lib.mkDefault false;
    };
    # NixOS's own tools for installing and changing a machine — nixos-install, -generate-config,
    # -enter, -option, -build-vms, and nixos-rebuild and nixos-version — are not on a TorqOS machine
    # (ADR-032): the image is installed already (the ISO installer has generate-config, on the ISO
    # only), torqcli reads and sets every option (`?`, `show`), and the broker builds and switches
    # with nix itself (modules/broker.nix `rebuildCommand`). bcache's tools are for a cache disk
    # TorqOS never sets up.
    system.tools = lib.genAttrs [ "nixos-install" "nixos-generate-config" "nixos-enter" "nixos-option" "nixos-build-vms" "nixos-rebuild" "nixos-version" ]
      (_: { enable = lib.mkDefault false; });
    boot.bcache.enable = lib.mkDefault false;
    # systemd's container-image importer (`machinectl pull-*`), and the GnuPG and LDAP libraries it
    # brings (~18 MB): containers come from the configuration (`virtualisation …`), not from it.
    systemd.services.systemd-importd.enable = lib.mkDefault false;

    system.stateVersion = lib.mkDefault "26.05";
  };
}
