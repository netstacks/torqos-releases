{ config, lib, pkgs, ... }:

let
  cfg = config.torq.desktop;
  tuigreet = pkgs.tuigreet or pkgs.greetd.tuigreet;
in
{
  options.torq.desktop.enable = lib.mkOption {
    type = lib.types.bool;
    # Off: TorqOS is CLI only for now (ADR-019); kept to bring a desktop back later.
    default = false;
    description = "A desktop: niri (Wayland) + Quickshell. Out of scope for now (ADR-019); off by default.";
  };

  config = lib.mkIf cfg.enable {
    # A desktop's network: Wi-Fi and the rest, with its own applet (base.nix uses systemd's).
    networking.networkmanager.enable = true;
    networking.useNetworkd = false;
    # Wayland only — no X server.
    services.xserver.enable = false;

    programs.niri.enable = true;

    services.greetd = {
      enable = true;
      settings.default_session.command =
        "${tuigreet}/bin/tuigreet --time --remember --cmd niri-session";
    };

    hardware.graphics.enable = true;

    security.rtkit.enable = true;
    services.pipewire = {
      enable = true;
      alsa.enable = true;
      pulse.enable = true;
    };

    fonts.packages = with pkgs; [ inter jetbrains-mono noto-fonts noto-fonts-color-emoji ];
    # What base.nix leaves out of a machine without a screen.
    fonts.fontconfig.enable = true;
    xdg = {
      autostart.enable = true;
      icons.enable = true;
      mime.enable = true;
      sounds.enable = true;
    };

    environment.systemPackages = with pkgs; [
      # TorqOS's icons (branding/), for what shows the system's logo (os-release's LOGO).
      config.torq.branding.package
      quickshell # TODO(M3): ship the TorqOS shell (shell/) instead of the bare toolkit
      foot
      fuzzel
      firefox
      wl-clipboard
    ];
  };
}
