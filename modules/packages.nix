# Installed software, as data. The package lists live in the machine's config repo as JSON so that
# tools (`torqcli`, later the agent) can change them by proposing a plain diff through the broker,
# instead of editing Nix code. They are leaves of the configuration tree (docs/CLI-DESIGN.md):
#
#   system/packages.json          system packages <name>          (system tier)
#   users/<name>/packages.json    users <name> packages <name>    (user tier)
#
# Each file is a JSON list of nixpkgs attribute paths, e.g. [ "htop", "python3Packages.numpy" ].
# A user's folder may hold only the user-tier leaves the broker allows (broker/src/policy.rs,
# USER_LEAVES); anything else there fails evaluation, so nothing planted can lie in wait for a
# later release to start reading it.
{ config, lib, pkgs, ... }:

let
  cfg = config.torq.packages;
  dir = cfg.configDir;

  # A package is a top-level nixpkgs attribute, or one level under one of these sets of ordinary
  # packages. Anything deeper (pkgsCross.*.hello, nixosTests.*, hello.tests.*) is refused: those are
  # derivations too, but a user-tier change could use them to start hour-long builds or VM tests
  # while holding the broker lock.
  packageSets = [
    "python3Packages" "haskellPackages" "nodePackages" "perlPackages" "rubyPackages" "luaPackages"
    "ocamlPackages" "rPackages" "vimPlugins" "emacsPackages" "gnomeExtensions" "kdePackages"
    "texlivePackages" "fishPlugins"
  ];
  isAttrName = s: builtins.match "[A-Za-z_][A-Za-z0-9_'+-]*" s != null;

  resolve = owner: name:
    let
      path = lib.splitString "." name;
      shaped = lib.all isAttrName path
        && (builtins.length path == 1 || (builtins.length path == 2 && builtins.elem (builtins.head path) packageSets));
      pkg = lib.attrByPath path null pkgs;
      what = "torq.packages: '${name}' (${owner})";
    in
    if !shaped then
      throw "${what} is not a package name: use a top-level nixpkgs attribute, or one level under ${lib.concatStringsSep ", " packageSets}"
    else if !(lib.isDerivation pkg) then
      throw "${what} is not a package in nixpkgs"
    # Unfree and not allowed: say so, and how to allow it (modules/unfree.nix), by the name nixpkgs
    # checks — which can differ from the attribute (`python3Packages.x` is `x`).
    else if !(pkg.meta.available or true) && lib.any (l: !(l.free or true)) (lib.toList (pkg.meta.license or [ ])) && !(pkg.meta.broken or false) then
      throw "${what} is unfree: allow it with `set torq unfree packages ${lib.getName pkg}`, or every unfree package with `set torq unfree all true`"
    else if !(pkg.meta.available or true) then
      throw "${what} is not available here: broken, insecure, or unsupported on ${pkgs.stdenv.hostPlatform.system}"
    else
      pkg;

  readList = file: if builtins.pathExists file then lib.importJSON file else [ ];

  # Same shape and leaves the broker treats as user tier (broker/src/policy.rs, is_user_data).
  isUserName = s: builtins.match "[a-z_][a-z0-9_-]*" s != null;
  # A leaf is added only together with the typed setting that reads it: packages.json (this module).
  # One list for the broker and the modules: broker/user-leaves.json
  # (the broker's own test checks its USER_LEAVES against it).
  userLeaves = lib.importJSON ../broker/user-leaves.json;
  usersDir = dir + "/users";
  entries = if dir != null && builtins.pathExists usersDir then builtins.readDir usersDir else { };

  # users/<name>/ folders, checked: a valid name, and nothing in them but allow-listed regular files.
  # Other top-level regular files in users/ are left alone (e.g. a <name>.nix module the config's
  # flake imports, which is system tier), except the old flat `<name>.packages.json`, which would
  # otherwise be silently ignored. A symlink there is refused rather than ignored: nothing planted
  # may lie in wait.
  userDirs = lib.mapAttrs (name: _:
    let
      leaves = builtins.readDir (usersDir + "/${name}");
      unknown = lib.attrNames (lib.filterAttrs (leaf: _: !(builtins.elem leaf userLeaves)) leaves);
      notFiles = lib.attrNames (lib.filterAttrs (_: type: type != "regular") leaves);
    in
    if !(isUserName name) then
      throw "torq: users/${name}/ does not name a valid user (a lowercase login name)"
    else if unknown != [ ] then
      throw "torq: users/${name}/${lib.head unknown} is not a setting (a user's folder holds only ${lib.concatStringsSep ", " userLeaves})"
    else if notFiles != [ ] then
      throw "torq: users/${name}/${lib.head notFiles} must be a regular file, not a ${leaves.${lib.head notFiles}}"
    else
      leaves
  ) (lib.filterAttrs (_: type: type == "directory") entries);
  oldLayout = lib.filterAttrs (n: type: type == "regular" && lib.hasSuffix ".packages.json" n) entries;
  links = lib.attrNames (lib.filterAttrs (_: type: type == "symlink") entries);
in
{
  options.torq.cli.package = lib.mkOption {
    type = lib.types.package;
    description = "The `torq` command line (set by the TorqOS flake).";
  };

  options.torq.packages = {
    configDir = lib.mkOption {
      type = lib.types.nullOr lib.types.path;
      default = null;
      description = ''
        The machine's config repo. When set, `system/packages.json` and `users/<name>/packages.json`
        in it are read into `system` and `user`. Set by `torq.lib.mkSystem { configDir = ./.; }`.
      '';
    };
    system = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "nixpkgs attribute paths installed system-wide.";
    };
    userLeaves = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      readOnly = true;
      default = userLeaves;
      description = "The files a users/<name>/ folder may hold (the broker's USER_LEAVES; read-only).";
    };
    user = lib.mkOption {
      type = lib.types.attrsOf (lib.types.listOf lib.types.str);
      default = { };
      description = "Per-user nixpkgs attribute paths, keyed by user name.";
    };
  };

  config = {
    torq.packages.system = lib.mkIf (dir != null) (readList (dir + "/system/packages.json"));
    torq.packages.user = lib.mkIf (dir != null) (
      lib.mapAttrs (name: _: lib.importJSON (usersDir + "/${name}/packages.json"))
        (lib.filterAttrs (_: leaves: leaves ? "packages.json") userDirs)
    );

    assertions =
      lib.mapAttrsToList (user: _: {
        assertion = config.users.users.${user}.isNormalUser or false;
        message = "torq: users/${user}/ names '${user}', which is not a normal user on this machine.";
      }) userDirs
      ++ lib.mapAttrsToList (n: _: {
        assertion = false;
        message = "torq: users/${n} is the old layout; move it to users/${lib.removeSuffix ".packages.json" n}/packages.json.";
      }) oldLayout
      ++ map (n: {
        assertion = false;
        message = "torq: users/${n} is a symlink; a user's settings must be a real folder (users/<name>/).";
      }) links;

    torq.schema = {
      users = {
        kind = "list";
        help = "People with an account on this machine";
        type = "user-name";
        # The machine's normal users whose names the broker treats as user tier (isUserName): the
        # only names `users <name>` may take. Names only — not their descriptions (full names).
        values = lib.genAttrs
          (lib.filter isUserName (lib.attrNames (lib.filterAttrs (_: u: u.isNormalUser) config.users.users)))
          (_: "");
      };
      "users <name>" = { kind = "container"; help = "One person's own settings"; tier = "user"; };
      "system packages" = {
        kind = "leaf-list";
        help = "Software installed for everyone (nixpkgs names)";
        file = "system/packages.json";
        type = "package";
      };
      "users <name> packages" = {
        kind = "leaf-list";
        help = "Software installed for this person only";
        file = "users/<name>/packages.json";
        tier = "user";
        type = "package";
      };
    };

    environment.systemPackages = [ config.torq.cli.package ] ++ map (resolve "system") cfg.system;
    users.users = lib.mapAttrs (user: names: { packages = map (resolve "user ${user}") names; }) cfg.user;
  };
}
