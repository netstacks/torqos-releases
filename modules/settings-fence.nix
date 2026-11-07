# What the configuration tree may never change (docs/CONFIG-IS-THE-DEVICE.md). Enforced when the
# system is evaluated (modules/nixos-settings.nix), so a configuration whose tree breaks it does not
# build, whoever proposed it; exported in /etc/torq/nixos-schema.json for torqcli to say so early.
#
# Two rules:
#  1. The tree is data. A key starting with `_` (NixOS's `_type` turns a set into mkMerge, mkForce,
#     mkIf…; `_module` is the module system's own) is refused anywhere, lists included.
#  2. The broker's own inputs are not the tree's: the commands and paths it runs as root, what its
#     settings file and PATH are built from, its units, its files in /etc (by where they land, not by
#     their key), and plumbing that could swap any package.
#
# This is not the boundary for the AI. Settings that run code as root (a service's script, sudo
# rules, wrappers, tmpfiles, binary caches, groups) are ordinary system-tier power when a person
# approves them; what an agent may propose at all is a default-deny allow-list at the broker, keyed on
# its identity (KNOWN-GAPS #4, M3). Changing what is fenced here is a change to TorqOS itself, or a
# reviewed .nix file.
#
#     NixOS's old names for a place (`users.extraUsers` for `users.users`) are refused too: through
#     one, a setting would be out of every rule written for its real name (audit of 2026-11-01, N2).
#
#  3. A person's password is never in the configuration: everyone on the machine can read it, and
#     its history keeps every value forever (a hash there can be cracked offline). Passwords live
#     in /var/lib/torq/secrets (modules/accounts.nix); `request system password` changes one.
#
# A pattern is a path; a segment ending in `*` matches any entry name starting with what precedes it.
{ lib }:

rec {
  # NixOS's aliases of a place the rules name: set at the real name, as torqcli does.
  aliases = [
    [ "users" "extraUsers" ]
    [ "users" "extraGroups" ]
  ];

  patterns = aliases ++ [
    [ "torq" "broker" ]
    [ "torq" "packages" ]
    [ "torq" "cli" "package" ]
    [ "torq" "schema" ]
    [ "torq" "nixosSchema" ]
    [ "torq" "releases" ]
    [ "torq" "branding" ]
    # The ISO installer's own mode (ADR-031): never a machine's.
    [ "torq" "installer" ]
    [ "nixpkgs" ]
    [ "nix" "package" ]
    [ "systemd" "package" ]
    [ "system" "build" ]
    [ "environment" "etc" "torq/*" ]
    [ "environment" "etc" "torq" ]
    [ "systemd" "services" "torq-*" ]
    [ "systemd" "timers" "torq-*" ]
    [ "systemd" "units" "torq-*" ]
    # The broker's door and its password gate (ADR-040): its socket, and the PAM stack that checks the
    # password (the audit of M3, L3).
    [ "systemd" "sockets" "torq-*" ]
    [ "security" "pam" "services" "torq-*" ]
    # And what decides who answers for whom there: su, sudo, and the lockout's count.
    [ "security" "pam" "services" "su" ]
    [ "security" "pam" "services" "sudo" ]
  ];

  # The settings TorqOS keeps in files of its own (torqcli's whole.rs MAP): one setting, one place —
  # set in nixos/ too, the tree would show one value and NixOS build another. `*`: any entry's name.
  elsewhere = [
    [ "networking" "hostName" ]
    [ "time" "timeZone" ]
    [ "environment" "systemPackages" ]
    [ "users" "users" "*" "packages" ]
    [ "torq" "services" "enabled" ]
  ];
  keptElsewhere = path: builtins.any (pat:
    builtins.length path == builtins.length pat && builtins.all lib.id (lib.zipListsWith segmentMatches pat path)
  ) elsewhere;

  # Rule 3: an account's password, in any of NixOS's forms.
  passwords = map (p: [ "users" "users" "*" p ]) [ "password" "hashedPassword" "initialPassword" "initialHashedPassword" "hashedPasswordFile" "passwordFile" ];

  segmentMatches = pat: seg:
    if lib.hasSuffix "*" pat then lib.hasPrefix (lib.removeSuffix "*" pat) seg else pat == seg;

  # Whether `path` (a list of words) is, or is below, one of `pats`.
  matchesAny = pats: path: builtins.any (pat:
    builtins.length path >= builtins.length pat
    && builtins.all lib.id (lib.zipListsWith segmentMatches pat (lib.take (builtins.length pat) path))
  ) pats;

  # Whether `path` is, or is below, a fenced setting.
  fenced = matchesAny patterns;

  # Where an /etc entry lands: its `target`, else its name, as path segments — empty and `.`
  # segments dropped (`/torq`, `./torq`, `torq//x`). A `..` is kept, and refused (`problem`): where
  # `x/../torq/broker.json` lands is /etc/torq/broker.json (audit of 2026-11-03).
  etcSegments = name: e:
    let t = if builtins.isAttrs e && builtins.isString (e.target or null) then e.target else name;
    in lib.filter (seg: seg != "" && seg != ".") (lib.splitString "/" t);
  etcTarget = name: e: lib.concatStringsSep "/" (etcSegments name e);

  # The first problem in a value set under `path`: a key starting with `_`, a fenced setting, or an
  # /etc entry landing in torq/. null if none.
  problem = path: v:
    let
      here = lib.concatStringsSep "." path;
      underscore = lib.findFirst (k: lib.hasPrefix "_" k) null (if builtins.isAttrs v then builtins.attrNames v else [ ]);
      etc = path == [ "environment" "etc" ] && builtins.isAttrs v;
      landsInTorq = lib.findFirst (k: let t = etcTarget k v.${k}; in t == "torq" || lib.hasPrefix "torq/" t) null
        (if etc then builtins.attrNames v else [ ]);
      climbs = lib.findFirst (k: builtins.elem ".." (etcSegments k v.${k})) null (if etc then builtins.attrNames v else [ ]);
      inner = if builtins.isAttrs v then lib.mapAttrsToList (k: x: problem (path ++ [ k ]) x) v
        else if builtins.isList v then lib.imap0 (i: x: problem (path ++ [ "[${toString i}]" ]) x) v
        else [ ];
    in
    if underscore != null then "${here}.${underscore}: a key starting with _ is a NixOS module directive (mkForce, mkMerge, …), not a setting: the tree holds data only"
    else if matchesAny aliases path then "${here}: NixOS's old name for ${if lib.elemAt path 1 == "extraUsers" then "users.users" else "users.groups"}: set it there (one place, every rule)"
    else if keptElsewhere path then "${here}: kept in its own file by TorqOS (as torqcli writes it), not in nixos/"
    else if matchesAny passwords path then "${here}: a password is never in the configuration (everyone can read it, and its history keeps it): `request system password <user>` sets one"
    else if path == [ "specialisation" ] then "specialisation: a second system, out of every rule here, is not the tree's: put it in an attached .nix file (config-files.json), reviewed as code"
    else if fenced path then "${here}: not settable in the configuration tree (it reaches the broker or TorqOS itself)"
    else if climbs != null then "environment.etc.${climbs}: a `..` in where an /etc file lands: not settable in the configuration tree"
    else if landsInTorq != null then "environment.etc.${landsInTorq}: lands in /etc/torq, the broker's: not settable in the configuration tree"
    else lib.findFirst (p: p != null) null inner;
}
