# What system/host.json may hold (modules/host.nix), as plain functions so tests/tree.nix can test
# the rules themselves and not only NixOS's own checks behind them.
{ lib, tzdata }:

let
  # The zones tzdata itself lists (its index, the list `timedatectl list-timezones` shows): `Z <name>`
  # zones and `L <target> <name>` links, like US/Eastern. Not every file under zoneinfo is a zone:
  # posix/ and right/ hold copies (right/ counts leap seconds, putting the clock ~27 s off NTP),
  # leapseconds is data, posix/Europe is a folder. "Factory" is a zone but means "not set".
  index = lib.splitString "\n" (builtins.readFile "${tzdata}/share/zoneinfo/tzdata.zi");
  named = line:
    let words = lib.splitString " " line; in
    if lib.hasPrefix "Z " line then [ (builtins.elemAt words 1) ]
    else if lib.hasPrefix "L " line then [ (builtins.elemAt words 2) ]
    else [ ];
  zones = lib.genAttrs (lib.remove "Factory" (lib.concatMap named index)) (_: true);
in
{
  # RFC 1123: one label, letters, digits and hyphens, not starting or ending with a hyphen, at most
  # 63 characters. No dots (a fully qualified name is a domain setting), no underscores (NixOS's
  # own type allows them; DNS does not). Not empty: NixOS reads "" as "ask DHCP"; leave the key out
  # instead.
  hostNameOk = s: builtins.isString s && builtins.match "[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?" s != null;

  timeZoneOk = s: builtins.isString s && zones ? ${s};

  # For completion (the schema).
  timeZones = lib.attrNames zones;
}
