# Evaluation test: the configuration tree's data files (docs/CLI-DESIGN.md) are read as documented,
# and anything malformed fails evaluation — the modules are the authority on what is valid, whoever
# wrote the file. Each case is a tiny config repo under tests/tree/<case>/.
#
# Evaluation only (no VM): a failing case makes `nix flake check` fail with the list of mismatches.
{ pkgs, self }:

let
  inherit (pkgs) lib;

  # "template": the config repo template as a new machine has it, with its hardware file (written at
  # setup by nixos-generate-config) and its own owner placeholder.
  templateMachine = pkgs.runCommand "template-machine" { } ''
    cp -r ${../templates/config} $out && chmod -R u+w $out
    cp ${./tree/template-hardware.nix} $out/hardware-configuration.nix
    sed -i s/CHANGE-ME/dev/ $out/nixos/torq.json  # as setup does
  '';
  configOf = case: (self.lib.mkSystem {
    system = pkgs.stdenv.hostPlatform.system;
    configDir = if case == "template" then templateMachine else ./tree + "/${case}";
    modules = lib.optional (case != "template") { torq.user = "dev"; };
  }).config;

  # The value, or null if evaluating it fails (a throw).
  tryValue = v: let r = builtins.tryEval (builtins.deepSeq v v); in if r.success then r.value else null;

  host = case: let c = configOf case; in tryValue { host = c.networking.hostName; zone = c.time.timeZone; };

  # TorqOS's own failed assertions (the case's minimal system fails NixOS's, e.g. no fileSystems).
  torqFailures = c: map (a: a.message) (lib.filter (a: !a.assertion && lib.hasPrefix "torq:" a.message) c.assertions);
  # A person's folder is refused (an evaluation error, or a TorqOS assertion) when it holds a file
  # no typed setting reads (`users-bad-leaf`; a leftover cli.json: torqcli spoke two syntaxes before
  # ADR-021), a symlink, or names nobody on this machine.
  usersRefused = case: let c = configOf case; in tryValue c.torq.packages.user == null || torqFailures c != [ ];
  # Unfree software (modules/unfree.nix): refused, saying how to allow it; allowed by name, or all.
  unfreeInstalled = case: tryValue (builtins.any (p: lib.getName p == "unrar") (configOf case).environment.systemPackages);
  unfreeSaid = (builtins.tryEval (builtins.deepSeq (configOf "unfree-refused").environment.systemPackages null)).success;
  usersInvalid = [ "users-leftover-cli" "users-bad-leaf" "users-bad-symlink" "users-bad-user" ];

  # The tree's NixOS settings (nixos/*.json, modules/nixos-settings.nix) reach NixOS as they are,
  # package names as packages; a wrong value, package or setting fails evaluation.
  isPkg = want: ps: builtins.any (p: p.outPath == want.outPath) ps;
  nixosValues = case:
    let c = configOf case; in
    tryValue {
      docker = c.virtualisation.docker.enable;
      hello = isPkg pkgs.hello c.environment.systemPackages;
      rich = isPkg pkgs.python3Packages.rich c.environment.systemPackages;
      forceSSL = c.services.httpd.virtualHosts."example.com".forceSSL;
      root = toString c.services.httpd.virtualHosts."example.com".documentRoot;
      ports = lib.sort lib.lessThan (lib.filter (p: p == 80 || p == 443) c.networking.firewall.allowedTCPPorts);
    };
  nixosExpect = { docker = true; hello = true; rich = true; forceSSL = true; root = "/var/www"; ports = [ 80 443 ]; };
  # Each bad case, forced only where it is bad (tryEval catches a throw, not a missing attribute).
  nixosInvalid = {
    nixos-bad-type = c: c.virtualisation.docker.enable;
    nixos-bad-package = c: c.services.httpd.package.outPath;
    nixos-bad-option = c: c.virtualisation.docker.enable;
    # The fence (modules/settings-fence.nix): what reaches the broker's guard is not the tree's.
    nixos-fence-broker = c: c.torq.broker.rebuildCommand;
    nixos-fence-etc = c: c.environment.etc."torq/broker.json".text;
    nixos-fence-unit = c: c.systemd.services.torq-broker-init.enable;
    # The bypasses the review of S3a demonstrated: module directives in the data, what the broker's
    # settings and PATH are built from, its units by another option, /etc by where it lands.
    nixos-fence-type-merge = c: c.torq.broker.rebuildCommand;
    nixos-fence-type-override = c: c.environment.etc."torq/broker.json".text;
    nixos-fence-type-list = c: map (p: p.outPath or p) c.environment.systemPackages;
    nixos-fence-build = c: c.system.build.nixos-rebuild;
    nixos-fence-nix = c: c.nix.package.outPath;
    nixos-fence-units = c: c.systemd.units."torq-broker-init.service".text;
    nixos-fence-etc-slash = c: c.environment.etc."/torq/broker.json".text;
    nixos-fence-etc-dot = c: c.environment.etc."./torq/broker.json".text;
    nixos-fence-etc-target = c: c.environment.etc.innocent.target;
    # A setting TorqOS keeps in its own file is not also in nixos/ (one setting, one place).
    nixos-elsewhere = c: c.networking.hostName;
    nixos-elsewhere-packages = c: map (p: p.outPath or p) c.environment.systemPackages;
    # A person's password is never in the configuration (settings-fence.nix, rule 3).
    nixos-password = c: c.users.users.dev.hashedPassword;
    # A file named as a module directive, NixOS's old name for users.users (audit of 2026-11-01).
    nixos-fence-underscore-file = c: c.torq.broker.rebuildCommand;
    nixos-fence-alias-users = c: c.users.users.dev.hashedPassword;
    # Config files: only .nix files inside the config repo (config-files.json).
    config-bad-dotdot = c: c.networking.hostName;
    config-bad-abs = c: c.networking.hostName;
    config-bad-missing = c: c.networking.hostName;
    config-bad-type = c: c.networking.hostName;
    config-bad-dot = c: c.networking.hostName;
    config-bad-space = c: c.networking.hostName;
    config-bad-flake = c: c.networking.hostName;
    config-bad-set = c: c.networking.hostName;
    config-bad-empty = c: c.networking.hostName;
  };
  # torqcli's tests read the fence from cli/src/nixos-fixture.json: it must be the modules' own.
  fenceFixture = (builtins.fromJSON (builtins.readFile ../cli/src/nixos-fixture.json)).fence
    == (import ../modules/settings-fence.nix { inherit lib; }).patterns;
  # A name that only looks like a fenced one is not fenced.
  fenceOk = tryValue (configOf "nixos-fence-ok").environment.etc."motd-torq/x".text;
  nixosGood = nixosValues "nixos-good";
  nixosMismatches =
    lib.optional (nixosGood != nixosExpect) "nixos-good: got ${builtins.toJSON nixosGood}"
    ++ lib.optional ((configOf "host-empty").virtualisation.docker.enable) "host-empty: docker on without a nixos/ file"
    ++ lib.optional (fenceOk != "hi") "nixos-fence-ok: motd-torq/x is not fenced, got ${builtins.toJSON fenceOk}"
    ++ lib.optional (!fenceFixture) "cli/src/nixos-fixture.json: its fence is not modules/settings-fence.nix's"
    ++ lib.optional (tryValue (configOf "config-good").services.httpd.virtualHosts."from-a-file".documentRoot != "/srv/file") "config-good: the attached file did not take effect"
    ++ lib.optional (tryValue (configOf "config-good").networking.domain != "from-the-top") "config-good: the file attached at the top did not take effect"
    # Several files at one place (a list): each takes effect.
    ++ lib.optional (tryValue (configOf "config-good-list").networking.domain != "from-the-top") "config-good-list: the first file did not take effect"
    ++ lib.optional (tryValue (configOf "config-good-list").networking.search != [ "from-a-second-file" ]) "config-good-list: the second file did not take effect"
    # A machine keeps the sources it is built from (flake.nix, lib.mkSystem).
    ++ lib.optional (!(builtins.all (s: builtins.elem (toString s) (map toString (configOf "template").system.extraDependencies)) [ self.outPath pkgs.path ]))
      "template: the system does not keep its TorqOS and nixpkgs sources"
    # The template is a whole machine: data, its hardware attached, nothing else, no desktop.
    ++ lib.optional (tryValue (configOf "template").system.build.toplevel.drvPath == null) "template: does not evaluate as a machine"
    ++ lib.optional (tryValue (configOf "template").fileSystems."/".device != "/dev/vda1") "template: its hardware file is not attached"
    ++ lib.optional (tryValue (configOf "template").torq.desktop.enable != false) "template: the desktop is on"
    # The root account is locked: a config file cannot give it a password (modules/accounts.nix).
    ++ lib.optional (!(lib.any (lib.hasInfix "root never has a password") (torqFailures (configOf "config-root-password")))) "config-root-password: root got a password"
    # TorqOS from a folder (path:) only in dev mode, by the configuration's own lock (modules/dev.nix).
    ++ lib.optional (!(lib.any (lib.hasInfix "comes from a folder") (torqFailures (configOf "dev-folder-off")))) "dev-folder-off: TorqOS from a folder without dev mode was not refused"
    ++ lib.concatMap (case: lib.optional (lib.any (lib.hasInfix "comes from a folder") (torqFailures (configOf case))) "${case}: refused, but may come from there") [ "dev-folder-on" "dev-store" ]
    ++ lib.concatLists (lib.mapAttrsToList (case: probe:
      lib.optional (tryValue (probe (configOf case)) != null) "${case}: evaluated, but must be refused") nixosInvalid);

  defaults = host "host-empty";
  expect = {
    host-good = { host = "my-laptop"; zone = "Europe/Berlin"; };
    host-utc = { host = defaults.host; zone = "UTC"; };
    host-ok-63 = { host = lib.concatStrings (lib.replicate 63 "a"); zone = defaults.zone; };
    host-link-zone = { host = defaults.host; zone = "US/Eastern"; };
    template = { host = "torq"; zone = "America/New_York"; };
  };
  invalid = [
    "host-bad-hyphen" "host-bad-dot" "host-bad-long" "host-bad-number"
    "host-bad-area" "host-bad-escape" "host-bad-dotdot" "host-bad-nowhere"
    "host-bad-key" "host-bad-list"
    # Files under zoneinfo that are not zones tzdata lists; "not set"; null; "ask DHCP"; and an
    # underscore, which NixOS's own type allows: only host-rules.nix refuses it.
    "host-bad-posix-dir" "host-bad-right" "host-bad-leapseconds" "host-bad-factory"
    "host-bad-null" "host-bad-empty-name" "host-bad-underscore"
  ];

  # The rules themselves, not only NixOS's checks behind them.
  rules = import ../modules/host-rules.nix { inherit lib; inherit (pkgs) tzdata; };
  ruleCases = [
    [ "hostNameOk" "torq" true ] [ "hostNameOk" "a-1" true ] [ "hostNameOk" "my_laptop" false ]
    [ "hostNameOk" "-a" false ] [ "hostNameOk" "a-" false ] [ "hostNameOk" "a.b" false ]
    [ "hostNameOk" "" false ] [ "hostNameOk" 5 false ] [ "hostNameOk" (lib.concatStrings (lib.replicate 64 "a")) false ]
    [ "timeZoneOk" "UTC" true ] [ "timeZoneOk" "Europe/Berlin" true ] [ "timeZoneOk" "US/Eastern" true ]
    [ "timeZoneOk" "posix/Europe" false ] [ "timeZoneOk" "posix/Europe/Berlin" false ] [ "timeZoneOk" "right/UTC" false ]
    [ "timeZoneOk" "leapseconds" false ] [ "timeZoneOk" "Factory" false ] [ "timeZoneOk" "America" false ]
    [ "timeZoneOk" "europe/berlin" false ] [ "timeZoneOk" "../etc/passwd" false ] [ "timeZoneOk" null false ]
  ];
  ruleMismatches = lib.concatMap (c:
    let f = builtins.elemAt c 0; arg = builtins.elemAt c 1; want = builtins.elemAt c 2; in
    lib.optional (rules.${f} arg != want) "${f} ${builtins.toJSON arg}: want ${builtins.toJSON want}"
  ) ruleCases;

  # The schema (modules/schema.nix) is consistent: every node's parent exists, files and keys are
  # where the kind says, `users <name> …` is user tier and nothing else is, and the user-tier leaves
  # it describes are exactly the leaves a users/<name>/ folder may hold — no setting without a node,
  # no node without a reader.
  schemaConfig = configOf "schema-good";
  schema = builtins.fromJSON schemaConfig.environment.etc."torq/schema.json".text;
  nodes = schema.nodes;
  parentOf = path: lib.concatStringsSep " " (lib.init (lib.splitString " " path));
  underUser = path: lib.hasPrefix "users <name>" path;
  schemaMismatches = lib.concatLists (lib.mapAttrsToList (path: n:
    let has = k: n ? ${k}; in
    lib.optional (lib.length (lib.splitString " " path) > 1 && !(nodes ? ${parentOf path})) "schema ${path}: no parent node"
    ++ lib.optional (n.kind == "leaf" && !(has "file" && has "key")) "schema ${path}: a leaf needs file and key"
    ++ lib.optional (n.kind == "leaf-list" && !(has "file") || n.kind == "leaf-list" && has "key") "schema ${path}: a leaf-list is a whole file"
    ++ lib.optional (builtins.elem n.kind [ "container" "list" ] && (has "file" || has "key")) "schema ${path}: a ${n.kind} has no file"
    ++ lib.optional ((n.tier == "user") != underUser path) "schema ${path}: tier ${n.tier}"
    ++ lib.optional (has "file" && underUser path && !(lib.hasPrefix "users/<name>/" n.file)) "schema ${path}: file outside users/<name>/"
    ++ lib.optional (has "file" && !(underUser path) && lib.hasPrefix "users/" n.file) "schema ${path}: system tier inside users/"
  ) nodes)
  # Each setting is in exactly one place: no two nodes share a (file, key), and a leaf-list's file is
  # its own (a leaf-list is the whole file).
  ++ (let
    withFile = lib.filter (n: n ? file) (lib.attrValues nodes);
    places = map (n: "${n.file}#${n.key or "*"}") withFile;
    listFiles = map (n: n.file) (lib.filter (n: n.kind == "leaf-list") withFile);
    sharedLists = lib.filter (f: lib.count (n: n.file == f) withFile > 1) listFiles;
  in lib.optional (lib.length (lib.unique places) != lib.length places) "schema: two nodes share a file and key"
    ++ lib.optional (sharedLists != [ ]) "schema: a leaf-list's file is also used elsewhere: ${toString sharedLists}")
  # `users` offers only names the broker treats as user tier, and no descriptions (full names).
  ++ (let
    withOdd = (self.lib.mkSystem {
      system = pkgs.stdenv.hostPlatform.system;
      configDir = ./tree/schema-good;
      modules = [ {
        torq.user = "dev";
        users.users.dev.description = "Secret Person"; # a listed user's full name must not be copied
        users.users."Bad.Name".isNormalUser = true;
      } ];
    }).config;
    users = (builtins.fromJSON withOdd.environment.etc."torq/schema.json".text).nodes.users;
  in lib.optional (users.values ? "Bad.Name" || !(users.values ? dev)) "schema: users ${builtins.toJSON users.values}"
    ++ lib.optional (lib.hasInfix "Secret" (builtins.toJSON users)) "schema: users carries descriptions"
    ++ lib.optional ((users.type or null) != "user-name") "schema: users has no name type")
  ++ (let
    described = lib.unique (map (n: lib.removePrefix "users/<name>/" n.file)
      (lib.filter (n: n ? file && n.tier == "user") (lib.attrValues nodes)));
    allowed = schemaConfig.torq.packages.userLeaves;
  in lib.optional (lib.sort lib.lessThan described != lib.sort lib.lessThan allowed)
    "schema: user-tier files ${builtins.toJSON described} != allowed user leaves ${builtins.toJSON allowed}")
  ++ lib.optional (!(nodes.users.values ? dev)) "schema: users lists the machine's normal users (dev)"
  ++ lib.optional (!(nodes."system time-zone".values ? "Europe/Berlin") || nodes."system time-zone".values ? Factory)
    "schema: time zones are tzdata's list"
  ++ lib.optional (lib.attrNames nodes.services.values != lib.attrNames schemaConfig.torq.services.catalog)
    "schema: services are the catalog";

  # torqcli's tests parse a copy of this schema (cli/src/schema-fixture.json): it must be the real one.
  # After a schema change, regenerate it (the command is in cli/src/tree.rs's tests).
  fixtureMismatch = lib.optional (builtins.fromJSON (builtins.readFile ../cli/src/schema-fixture.json) != schema)
    "cli/src/schema-fixture.json is not the schema the modules produce: regenerate it";

  mismatches =
    ruleMismatches
    ++ fixtureMismatch
    ++ schemaMismatches
    ++ lib.optional (defaults == null) "host-empty: does not evaluate"
    ++ lib.concatLists (lib.mapAttrsToList (case: want:
      let got = host case; in lib.optional (got != want) "${case}: got ${builtins.toJSON got}, want ${builtins.toJSON want}"
    ) expect)
    ++ lib.concatMap (case: lib.optional (host case != null) "${case}: evaluated, but must be refused") invalid
    ++ lib.concatMap (case: lib.optional (!(usersRefused case)) "${case}: evaluated, but must be refused") usersInvalid
    ++ lib.optional (unfreeInstalled "unfree-refused" != null || unfreeSaid) "unfree-refused: an unfree package not allowed evaluated"
    ++ lib.optional (unfreeInstalled "unfree-named" != true) "unfree-named: an unfree package allowed by name was not installed"
    ++ lib.optional (unfreeInstalled "unfree-all" != true) "unfree-all: an unfree package was not installed with all allowed"
    ++ lib.optional (usersRefused "schema-good") "schema-good: a person's own packages.json must evaluate"
    ++ nixosMismatches;
in
if mismatches != [ ] then
  throw "tree test:\n  ${lib.concatStringsSep "\n  " mismatches}"
else
  pkgs.runCommand "torq-tree-test" { } ''
    echo "rules: ${toString (builtins.length ruleCases)} cases" > $out
    echo "schema: ${toString (builtins.length (lib.attrNames nodes))} nodes consistent" >> $out
    echo "host: ${toString (builtins.length invalid)} invalid cases refused, ${toString (builtins.length (lib.attrNames expect) + 1)} valid ones read" >> $out
    echo "users: ${toString (builtins.length usersInvalid)} invalid folders refused, schema-good read" >> $out
    echo "unfree: refused unless allowed, by name or all" >> $out
    echo "nixos: ${toString (builtins.length (lib.attrNames nixosInvalid))} invalid cases refused, nixos-good read" >> $out
  ''
