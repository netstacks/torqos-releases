# TorqOS's look, as a system uses it (README.md here): its icons in the freedesktop layout
# (share/icons/hicolor: `torqos`, and `distributor-logo-torqos`, the name /etc/os-release gives as
# LOGO), and its terminal logos and fastfetch configuration (share/torqos).
{ lib, stdenvNoCC }:

stdenvNoCC.mkDerivation {
  pname = "torqos-branding";
  version = "1.0";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [ ./icons ./terminal ];
  };

  installPhase = ''
    mkdir -p $out/share/icons $out/share/torqos
    cp -r icons/hicolor $out/share/icons/
    cp terminal/* $out/share/torqos/
  '';

  meta = {
    description = "TorqOS's icons, terminal logos and fastfetch configuration";
    license = lib.licenses.asl20;
    platforms = lib.platforms.all;
  };
}
