# Passwords (docs/INSTALL.md; SECURITY-MODEL.md, "Accounts and passwords"): never in the config repo,
# which every user can read and which keeps its history forever (a hash there can be cracked
# offline). The configuration owns the accounts (users.mutableUsers = false); each person's password
# is a hash in a file only the system reads, /var/lib/torq/secrets/passwords/<user>, that their
# account points at. No file: no password — an account that logs in by key only, or not at all.
# First-boot setup writes the owner's; `request system password` changes one (ledgered, never the hash).
{ config, lib, ... }:

let
  dir = "/var/lib/torq/secrets/passwords";
  people = config.torq.accounts.people;
  # Set once the passwords of a machine from before have been carried over (below).
  carried = "/var/lib/torq/secrets/carried-over";
in
{
  # Every person's account (isNormalUser) points at its file, unless the tree says otherwise.
  options.users.users = lib.mkOption {
    type = lib.types.attrsOf (lib.types.submodule ({ name, config, ... }: {
      config.hashedPasswordFile = lib.mkIf config.isNormalUser (lib.mkDefault "${dir}/${name}");
    }));
  };

  options.torq.accounts.people = lib.mkOption {
    type = lib.types.listOf lib.types.str;
    readOnly = true;
    default = lib.attrNames (lib.filterAttrs (name: u: u.isNormalUser && u.hashedPasswordFile == "${dir}/${name}") config.users.users);
    description = "The people whose password is their file in ${dir} (read-only): the accounts `request system password` changes.";
  };

  config = {
    # The root account is locked (SECURITY-MODEL.md): nothing gives it a password, however it is
    # written (audit of 2026-11-01, N2).
    assertions = [
      {
        assertion = lib.all (k: (config.users.users.root.${k} or null) == null) [ "password" "hashedPassword" "initialPassword" "initialHashedPassword" "hashedPasswordFile" ];
        message = "torq: root never has a password (it is locked; a person logs in as themselves, then sudo).";
      }
    ];

    users.mutableUsers = false;

    # A machine from before this (its passwords set with `passwd`, in /etc/shadow only): each
    # person's password is carried into their file before the accounts are written — otherwise
    # those would lock every account without a file, its owner's too. For whoever has no file yet
    # and a real password, never a locked or empty one; and once, on the first activation with this
    # module (a new machine's too): from then on the files are what counts, and a file removed is a
    # password removed.
    system.activationScripts.torqPasswords = {
      deps = [ "specialfs" ];
      text = ''
        install -d -m 0700 /var/lib/torq/secrets ${dir}
        if [ ! -e ${carried} ]; then
          for person in ${lib.escapeShellArgs people}; do
            file=${dir}/$person
            [ -e "$file" ] && continue
            while IFS=: read -r name hash _; do
              if [ "$name" = "$person" ]; then
                case "$hash" in
                  '$'*) (umask 077; printf '%s\n' "$hash" > "$file.new") && mv "$file.new" "$file" ;;
                esac
              fi
            done < /etc/shadow
          done
          touch ${carried}
        fi
      '';
    };
    system.activationScripts.users.deps = [ "torqPasswords" ];
    systemd.tmpfiles.rules = [
      "d /var/lib/torq/secrets 0700 root root -"
      "d ${dir} 0700 root root -"
    ];
  };
}
