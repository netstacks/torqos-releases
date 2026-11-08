# The declared backup (ADR-038): NixOS's own `services.restic.backups`, set in the configuration tree as
# any NixOS setting is (`set services restic backups nas repository sftp:backup@nas:/torq5`). TorqOS adds
# three things, and nothing else of its own:
#
#  1. Its key is never in the repository. Each backup's `passwordFile` is
#     /var/lib/torq/secrets/backup/<name>, root's alone, which `request system backup key <name>` writes
#     (the broker; ledgered without the key). The tree may not point it elsewhere (settings-fence.nix,
#     rule 3), as a person's password file is not the tree's either.
#  2. What it holds: /home and /var/lib, unless the tree says otherwise; and never
#     /var/lib/torq/secrets, unless the tree's `paths` name it. Those are every person's password hash,
#     the remote's key and every backup's own key: a backup is kept where the machine's own protections
#     are not, and its key in it would be no protection at all. Nor the broker's room for a restore in
#     progress: a copy of someone's data, kept from them.
#  3. The broker knows each backup's restic, with its repository and key already set (as NixOS's own
#     `restic-<name>` wrapper does), for `show system backups` and `request system restore`.
{ config, lib, pkgs, ... }:

let
  keys = "/var/lib/torq/secrets/backup";
  secrets = "/var/lib/torq/secrets";
  # Where the broker keeps a restore in progress (broker/src/backup.rs ROOMS).
  rooms = "/var/cache/torq/restoring";
  stateDir = config.torq.broker.stateDir;
  backups = config.services.restic.backups;
  # Whether `paths` names the secrets, or something in them.
  namesSecrets = paths: lib.any (p: p == secrets || lib.hasPrefix "${secrets}/" p) (if paths == null then [ ] else paths);
  # Restic for backup `name`, as NixOS's wrapper is (nixos/modules/services/backup/restic.nix): the
  # unit's environment, its environment file, its options.
  restic = name: b:
    let unit = config.systemd.services."restic-backups-${name}"; in
    pkgs.writeShellScript "torq-restic-${name}" ''
      set -a
      ${lib.optionalString (b.environmentFile != null) "source ${lib.escapeShellArg b.environmentFile}"}
      ${lib.toShellVars (lib.mapAttrs (_: v: "${v}") (lib.filterAttrs (n: v: v != null && n != "PATH") unit.environment))}
      PATH=${unit.environment.PATH or ""}:$PATH
      # extraOptions as NixOS's own wrapper and unit write them, shell words (`sftp.command='ssh …'`, its
      # example): quoted here, they would mean something else here than in the backup itself.
      exec ${lib.getExe b.package}${lib.concatMapStrings (a: " -o ${a}") b.extraOptions} "$@"
    '';
in
{
  options.services.restic.backups = lib.mkOption {
    type = lib.types.attrsOf (lib.types.submodule ({ name, config, ... }: {
      config = {
        passwordFile = lib.mkDefault "${keys}/${name}";
        # A backup of a command's output (`command`) holds no paths: NixOS refuses both at once.
        paths = lib.mkIf (config.command == [ ]) (lib.mkDefault [ "/home" "/var/lib" ]);
        # The broker's rooms, and what a request left in its state from before requests moved to /run.
        exclude = [ rooms "${stateDir}/request-*" ] ++ lib.optional (!namesSecrets config.paths) secrets;
      };
    }));
  };

  # What broker.json says of them (modules/broker.nix): each backup's name, and its restic.
  options.torq.broker.backups = lib.mkOption {
    type = lib.types.attrsOf lib.types.str;
    readOnly = true;
    internal = true;
    default = lib.mapAttrs (name: b: "${restic name b}") backups;
    description = "Each declared backup's restic, with its repository and key set: for the broker.";
  };

  config = {
    assertions = lib.flatten (lib.mapAttrsToList (name: b: [
      {
        # The key's file is the name's, and the broker's commands take it as a word.
        assertion = builtins.match "[A-Za-z0-9][A-Za-z0-9_-]{0,63}" name != null;
        message = "services.restic.backups.${name}: a backup's name is letters, digits, - and _ (64 at most), starting with a letter or digit: it names its key's file.";
      }
      {
        assertion = b.passwordFile != "${keys}/${name}" || b.user == "root";
        message = "services.restic.backups.${name}: its key (${keys}/${name}) is root's alone, so the backup runs as root: user = \"${b.user}\" could not read it.";
      }
    ]) backups);
    systemd.tmpfiles.rules = [ "d ${keys} 0700 root root -" "d /var/cache/torq 0700 root root -" ];
  };
}
