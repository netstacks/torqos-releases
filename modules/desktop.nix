{ config, lib, pkgs, ... }:

let
  cfg = config.torq.desktop;
  # The lid, on a machine that must not suspend: dark and locked when it closes, as it was when it opens.
  # (`TORQ_BACKLIGHT`: for the test; acpid, as root, starts this with its own environment.)
  lidAction = pkgs.writeShellScript "torq-lid-action" ''
    PATH=${lib.makeBinPath [ pkgs.coreutils ]}
    backlights=''${TORQ_BACKLIGHT:-/sys/class/backlight}
    kept=/run/torq/lid-brightness
    mkdir -p "$kept"
    case "$1" in
      *close)
        for b in "$backlights"/*; do
          name=''${b##*/}
          # Closed twice: what it was before the first, not the dark one.
          [ -e "$kept/$name" ] || cp "$b/brightness" "$kept/$name" || continue
          echo 0 > "$b/brightness"
        done
        ${config.systemd.package}/bin/loginctl lock-sessions
        ;;
      *open)
        for b in "$backlights"/*; do
          name=''${b##*/}
          if [ -e "$kept/$name" ]; then
            cat "$kept/$name" > "$b/brightness"
            rm -f "$kept/$name"
          fi
        done
        ;;
    esac
  '';
  # COSMIC from the nixpkgs that has its latest release (ADR-045, amended); the release's nixpkgs keeps
  # the version it shipped with for six months, while COSMIC releases every few weeks.
  cosmicPkgs = import cfg.cosmicNixpkgs { localSystem = config.nixpkgs.hostPlatform.system; };
  # COSMIC's own packages and what its modules take with them; the rest of the system stays the release's.
  fromCosmic = lib.filter (n: lib.hasPrefix "cosmic-" n) (lib.attrNames cosmicPkgs)
    ++ [ "xdg-desktop-portal-cosmic" "pop-launcher" "pop-icon-theme" ];
in
{
  options.torq.desktop = {
    enable = lib.mkOption {
      type = lib.types.bool;
      # Off in the base: the desktop is a configuration (`torqos-desktop`, ADR-034), not the product.
      default = false;
      description = "TorqOS Desktop: COSMIC on Wayland with the cosmic-greeter (ADR-045). Off in the base.";
    };
    dimInsteadOfSuspend = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Never suspend: closing the lid locks the machine and turns the display fully down, and opening it brings the brightness back. For a laptop that cannot wake its display from suspend (some OLED panels).";
    };
    customCompile = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Allow TorqOS's own changes to COSMIC (ADR-054). Each makes this machine compile a COSMIC package itself instead of taking it prebuilt from NixOS's cache, at almost every update: cosmic-comp takes a long time on a laptop. Off: COSMIC as NixOS builds it, and every custom feature below refused.";
    };
    touchpadGestures = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "More touchpad gestures (ADR-054): three fingers up for the workspace overview (down closes it), three fingers down for the window switcher, four fingers pinched in for the app library. A change to COSMIC's compositor: needs `torq desktop customCompile true`.";
    };
    cosmicNixpkgs = lib.mkOption {
      type = lib.types.path;
      internal = true;
      description = "The nixpkgs COSMIC's packages, its NixOS modules and Mesa come from (flake.nix).";
    };
    settingsPackage = lib.mkOption {
      type = lib.types.package;
      internal = true;
      description = "TorqOS Settings (ADR-045 2), built from the nixpkgs COSMIC comes from (flake.nix).";
    };
  };

  # COSMIC's module from the newer nixpkgs sets `security.polkit.enablePkexecWrapper` (true), an option the
  # release's polkit module does not have: it installs setuid pkexec whenever polkit is on. Declared here
  # for that release only, and only as what it does: anything but true is refused.
  options.security = lib.optionalAttrs (lib.versionOlder lib.trivial.release "26.11") {
    polkit.enablePkexecWrapper = lib.mkOption {
      type = lib.types.bool;
      default = true;
      internal = true;
      description = "On NixOS 26.05, pkexec is installed setuid whenever polkit is on; this cannot change it.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = lib.optional (lib.versionOlder lib.trivial.release "26.11") {
      assertion = config.security.polkit.enablePkexecWrapper;
      message = "security.polkit.enablePkexecWrapper = false has no effect on NixOS 26.05: pkexec is installed with polkit.";
    } ++ [{
      # A custom COSMIC feature never turns on the machine's compiling COSMIC by itself (ADR-054).
      assertion = cfg.touchpadGestures -> cfg.customCompile;
      message = "torq desktop touchpadGestures changes COSMIC's compositor, which this machine then compiles itself at almost every update: turn that on first, `set torq desktop customCompile true`.";
    }];
    nixpkgs.overlays = [ (_: _: lib.genAttrs fromCosmic (n: cosmicPkgs.${n}) // lib.optionalAttrs (cfg.customCompile && cfg.touchpadGestures) {
      # Touchpad gestures COSMIC leaves to do: three fingers up for the workspace overview (down closes it),
      # down for the window switcher, four fingers pinched in for the app library. In the compositor, which
      # has the touchpad already, so no program needs the `input` group (it reads every keyboard too, the
      # owner's password with it). Built here, not taken from the cache; checked again at each COSMIC bump.
      cosmic-comp = cosmicPkgs.cosmic-comp.overrideAttrs (old: {
        patches = (old.patches or [ ]) ++ [ ./cosmic/cosmic-comp-gestures.patch ];
        # Built on the machine itself, a laptop as often as not: upstream's whole-program optimisation (`lto =
        # "fat"`, for its own build servers) with every core froze one with 22 GB until a reboot, twice. Thin
        # LTO and at most 8 jobs (0, from Nix, means every core) instead.
        env = (old.env or { }) // { CARGO_PROFILE_RELEASE_LTO = "thin"; };
        preBuild = (old.preBuild or "") + ''
          if [ "$NIX_BUILD_CORES" -eq 0 ] || [ "$NIX_BUILD_CORES" -gt 8 ]; then NIX_BUILD_CORES=8; fi
        '';
      });
    }) ];
    # The graphics driver from the same nixpkgs: COSMIC's compositor and the driver it loads must be one Mesa.
    hardware.graphics.package = cosmicPkgs.mesa;

    # COSMIC, and only COSMIC (ADR-045), through NixOS's own modules at the version nixpkgs pins. They
    # bring the compositor, the panel and its applets, COSMIC's own terminal, files, editor, launcher
    # and settings, the portal (xdg-desktop-portal-cosmic), the polkit agent (cosmic-osd), PipeWire,
    # Bluetooth, power profiles, geoclue and the keyring.
    services.desktopManager.cosmic.enable = true;
    services.displayManager.cosmic-greeter.enable = true;
    # TorqOS has its own first-boot setup, and the configuration owns what COSMIC's wizard would set (the
    # time zone, accounts): not a second one that writes around it.
    environment.cosmic.excludePackages = [ pkgs.cosmic-initial-setup ];
    # A wrong password in COSMIC's dialog counts toward the same lockout as one at a terminal (ADR-045 5): no
    # more guesses for changing where they are made.
    security.pam.services.polkit-1.text = config.security.pam.services.torq-approve.text;
    # polkit's helper runs sandboxed, with the file system read-only: without this it can neither count a wrong
    # password nor read a count another left. And its socket is open to every account by default: to people only,
    # or a service or the agent could spend the owner's tries.
    systemd.services."polkit-agent-helper@".serviceConfig.ReadWritePaths = [ "-/run/torq/faillock" ];
    systemd.sockets.polkit-agent-helper.socketConfig = {
      SocketMode = "0660";
      SocketGroup = "users";
    };
    services.desktopManager.cosmic.showExcludedPkgsWarning = false;

    # A machine that cannot wake its display must never go to sleep, by the lid or anything else (COSMIC's own
    # idle suspend included): sleep is refused, and the lid dims and locks instead.
    services.logind.settings.Login = lib.mkIf cfg.dimInsteadOfSuspend {
      HandleLidSwitch = "ignore";
      HandleLidSwitchExternalPower = "ignore";
      HandleLidSwitchDocked = "ignore";
    };
    systemd.sleep.settings.Sleep = lib.mkIf cfg.dimInsteadOfSuspend {
      AllowSuspend = "no";
      AllowHibernation = "no";
      AllowHybridSleep = "no";
      AllowSuspendThenHibernate = "no";
    };
    services.acpid.lidEventCommands = lib.mkIf cfg.dimInsteadOfSuspend ''${lidAction} "$1"'';
    environment.etc."torq/lid-action" = lib.mkIf cfg.dimInsteadOfSuspend { source = lidAction; };

    # A desktop's network: NetworkManager, for COSMIC's network applet and settings page, with iwd as its
    # Wi-Fi backend (ADR-046 2): one Wi-Fi stack, so networks joined at setup with iwctl stay joined. The
    # networks and their secrets are NetworkManager's and iwd's state, never the repository's (the fence).
    networking.networkmanager.enable = true;
    networking.networkmanager.wifi.backend = "iwd";
    networking.useNetworkd = false;

    # Applications by rung (ADR-037, amended): Flatpak on, which also brings the COSMIC Store. Its
    # installs are per user, in the person's home, with no password; Flathub is added for each person,
    # never system-wide (a system Flatpak is a tree setting). From Flathub's own repo file (from
    # dl.flathub.org, with its signing key), carried here: adding the remote needs no network, so a
    # login before Wi-Fi is up still gets it.
    services.flatpak.enable = true;
    systemd.user.services.torq-flathub = {
      description = "Flathub for this person's own Flatpaks (ADR-037)";
      wantedBy = [ "default.target" ];
      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${pkgs.flatpak}/bin/flatpak remote-add --user --if-not-exists flathub ${./flathub.flatpakrepo}";
      };
    };

    # Memory run short should end the program using it, not freeze the desktop: without swap the kernel
    # thrashes page cache and systemd-oomd, which reads swap pressure, is degraded. Compressed, in memory.
    zramSwap.enable = lib.mkDefault true;

    hardware.graphics.enable = true;

    # What base.nix leaves out of a machine without a screen.
    fonts.packages = with pkgs; [ inter jetbrains-mono noto-fonts noto-fonts-color-emoji ];
    fonts.fontconfig.enable = true;
    xdg = {
      autostart.enable = true;
      icons.enable = true;
      mime.enable = true;
      sounds.enable = true;
    };

    # A person's SSH tunnels (ADR-050): `torq-tunnel@<name>.service`, a user unit each person starts from the
    # tunnels applet, kept up by their own systemd while they are logged in. The tunnels are theirs, in their
    # home, as their Wi-Fi networks are: never the configuration's, and never the agent's to reach.
    systemd.packages = [ cfg.settingsPackage ];

    environment.systemPackages = [
      # TorqOS's icons (branding/), for what shows the system's logo (os-release's LOGO).
      config.torq.branding.package
      # TorqOS Settings talks to GitHub through it, for setting up a backup.
      pkgs.curl
      # TorqOS Settings (ADR-045 2): torqcli in a COSMIC window, the person's own, no privilege.
      cfg.settingsPackage
    ];
  };
}
