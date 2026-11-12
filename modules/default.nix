{
  disabledModules = [
    "services/desktop-managers/cosmic.nix"
    "services/display-managers/cosmic-greeter.nix"
  ];

  imports = [
    ./base.nix
    ./security.nix
    ./desktop.nix
    # COSMIC's NixOS modules, as nixos-unstable has them (ADR-045, amended): a newer COSMIC names packages and
    # settings the release's do not know. Carried here, not imported from that nixpkgs, so a machine without
    # the desktop never needs it (`checks.cosmic-modules` keeps them its copies; scripts/bump-cosmic moves them).
    ./cosmic/cosmic.nix
    ./cosmic/cosmic-greeter.nix
    ./broker.nix
    ./ai.nix
    ./agent.nix
    ./strict.nix
    ./dev.nix
    ./packages.nix
    ./unfree.nix
    ./setup.nix
    ./accounts.nix
    ./backup.nix
    ./secrets.nix
    ./host.nix
    ./shell.nix
    ./schema.nix
    ./nixos-schema.nix
    ./services.nix
    ./releases.nix
    ./branding.nix
    ./installer.nix
  ];
}
