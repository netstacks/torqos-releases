# The configuration tree's schema (ADR-016, docs/CLI-DESIGN.md): what torqcli knows about each
# setting, for parsing, `?` help, Tab completion and early errors. Each setting's module declares its
# own nodes next to the code that reads its file (`torq.schema.<path>`); the result is installed as
# /etc/torq/schema.json, so it always describes the *running* system.
#
# The schema is not the authority: the modules' checks are, at evaluation, whoever wrote the file.
#
# A node is keyed by its path in the tree, words separated by spaces, `<name>` for an entry of a
# list ("users <name> packages"). Fields:
#   kind     container | list (entries named by the user, e.g. users) | leaf | leaf-list
#   help     one line for `?`
#   file     leaf, leaf-list: the config repo file holding it; `<name>` as in the path
#   key      leaf: its key in that file's JSON object (a leaf-list is the whole file, a JSON list)
#   tier     system | user (what the broker will call a change to it)
#   values   the allowed values, if a closed set: { value = "help"; } (completion and checking)
#   type     otherwise what a value must look like: package | host-name | user-name (checked by
#            the module; for a list, what its entry names must look like — the shell checks a
#            name against it before putting it into a path)
#
# `set` and `delete` act on leaves (one key of one file) and leaf-lists (one element of one file's
# list). `delete` of a container or a list entry ("delete users alice") deletes every setting below
# it, each in its own file; nothing else maps a command to a file. A path has no other meaning: a
# leaf or leaf-list has no children, so no value can be read as a further node. (So per-service
# settings, `services ssh port 2222`, will turn `services` from a leaf-list into a list.)
{ config, lib, ... }:

let
  node = lib.types.submodule {
    options = {
      kind = lib.mkOption { type = lib.types.enum [ "container" "list" "leaf" "leaf-list" ]; };
      help = lib.mkOption { type = lib.types.str; };
      file = lib.mkOption { type = lib.types.nullOr lib.types.str; default = null; };
      key = lib.mkOption { type = lib.types.nullOr lib.types.str; default = null; };
      tier = lib.mkOption { type = lib.types.enum [ "system" "user" ]; default = "system"; };
      values = lib.mkOption { type = lib.types.nullOr (lib.types.attrsOf lib.types.str); default = null; };
      type = lib.mkOption { type = lib.types.nullOr (lib.types.enum [ "package" "host-name" "user-name" ]); default = null; };
    };
  };
  clean = lib.mapAttrs (_: n: lib.filterAttrs (_: v: v != null) n);
in
{
  options.torq.schema = lib.mkOption {
    type = lib.types.attrsOf node;
    default = { };
    description = "The configuration tree, for torqcli (installed as /etc/torq/schema.json).";
  };

  config = {
    # The container no single setting owns (`users` is modules/packages.nix's: it owns users/).
    torq.schema.system = { kind = "container"; help = "This machine"; };

    environment.etc."torq/schema.json".text = builtins.toJSON {
      version = 1;
      nodes = clean config.torq.schema;
    };
  };
}
