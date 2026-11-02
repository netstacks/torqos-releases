# The configuration tree's NixOS settings (docs/CONFIG-IS-THE-DEVICE.md, S2): every file
# `nixos/<name>.json` in the config repo holds the settings under NixOS's top-level `<name>`, as JSON
# shaped like the options themselves:
#
#   nixos/virtualisation.json   { "docker": { "enable": true } }
#   nixos/services.json         { "httpd": { "enable": true, "virtualHosts": { "example.com": { "forceSSL": true } } } }
#   nixos/environment.json      { "systemPackages": [ "htop" ] }
#
# torqcli writes them (`virtualisation docker enable true`); this module gives them to NixOS as they
# are, so NixOS's own types check every value — a wrong one fails evaluation, whoever wrote the file.
# The one translation: where a setting's type is a package, a name (`htop`, `python3Packages.rich`)
# becomes that package of this machine's nixpkgs.
#
# Given a path, not an option: which settings it defines must not depend on any setting (that would
# be infinite recursion), so `torq.lib.mkSystem` builds this module from `configDir` itself. Every
# file here is system tier: the broker's user tier is only `users/<name>/{packages,cli}.json`.
configDir:
{ options, pkgs, lib, ... }:

let
  dir = configDir + "/nixos";
  names = lib.optionals (configDir != null && builtins.pathExists dir) (
    map (f: lib.removeSuffix ".json" f) (
      builtins.filter (f: lib.hasSuffix ".json" f) (builtins.attrNames (builtins.readDir dir))
    )
  );
  read = name: builtins.fromJSON (builtins.readFile (dir + "/${name}.json"));

  fence = import ./settings-fence.nix { inherit lib; };
  # The tree may not change what reaches the broker's own guard (settings-fence.nix): refused here,
  # when the system is evaluated, whoever proposed it.
  checked = name: v:
    let p = fence.problem [ name ] v; in
    if p != null then throw "torq: ${p}" else v;

  package = name:
    lib.attrByPath (lib.splitString "." name) (throw "torq: '${name}' is not a package in this machine's nixpkgs") pkgs;

  # A value, by the type of the setting it is for.
  byType = t: v:
    let
      k = t.name or "";
      elem = t.nestedTypes.elemType or null;
    in
    if k == "package" && builtins.isString v then package v
    else if (k == "listOf") && elem != null && builtins.isList v then map (byType elem) v
    else if (k == "nullOr") && elem != null && v != null then byType elem v
    else if (k == "attrsOf" || k == "lazyAttrsOf") && elem != null && builtins.isAttrs v then lib.mapAttrs (_: byType elem) v
    else if k == "submodule" && builtins.isAttrs v then under (t.getSubOptions [ ]) v
    else v;

  # Values below a set of options: each by its option's type; what names no option is left as it is,
  # for NixOS to refuse ("option does not exist").
  under = opts: v:
    if lib.isOption opts then byType opts.type v
    else if builtins.isAttrs v then lib.mapAttrs (k: x: if builtins.isAttrs opts && opts ? ${k} then under opts.${k} x else x) v
    else v;
  # The tree's config files (docs/CONFIG-IS-THE-DEVICE.md): `config-files.json` maps where they are
  # attached in the tree (its path, words separated by spaces; "" is the top) to a list of ordinary
  # NixOS module files in the config repo (one name, the first form, is a list of one). Inside the repo only — relative, no `..`, a `.nix` file — so the machine never depends
  # on a file nothing tracks. A config file is plain NixOS and may set anything: it is system tier, and
  # shown as its raw diff before anyone commits it.
  attached =
    let f = if configDir != null then configDir + "/config-files.json" else null; in
    if f != null && builtins.pathExists f then builtins.fromJSON (builtins.readFile f) else { };
  # The same rule as torqcli's (cli/src/attach.rs `valid`): plain path characters, no empty, `.` or
  # `..` part, not TorqOS's own flake.nix.
  configFile = at: rel:
    let
      ok = builtins.isString rel && lib.hasSuffix ".nix" rel
        && builtins.match "[A-Za-z0-9._/-]+" rel != null
        && !(lib.any (p: p == "" || p == "." || p == "..") (lib.splitString "/" rel))
        && rel != "flake.nix";
      path = configDir + "/${rel}";
      shown = if builtins.isString rel then rel else "(not a file name)";
      place = if at == "" then "the top" else "`${at}`";
    in
    if !ok then throw "torq: config file for ${place}: '${shown}' must be a .nix file inside the config repo (like services/httpd.nix: relative, no ..)"
    else if !builtins.pathExists path then throw "torq: config file for ${place}: ${rel} is not in the config repo"
    else path;
in
{
  imports = lib.concatLists (lib.mapAttrsToList (at: rels:
    if rels == [ ] then throw "torq: config-files.json: ${if at == "" then "the top" else "`${at}`"}: an empty list (no files: no entry)"
    else map (configFile at) (lib.toList rels)) attached);
  # A file is one of NixOS's top-level settings, by name: never a module directive (`_type.json`,
  # whose value would make the whole of nixos/ an mkMerge or mkOverride: audit of 2026-11-01, N1),
  # never a name no option has.
  config = lib.genAttrs names (name:
    if lib.hasPrefix "_" name || !(options ? ${name})
    then throw "torq: nixos/${name}.json: '${name}' is not one of NixOS's settings (a file here is one: networking.json, services.json, …)"
    else under options.${name} (checked name (read name)));
}
