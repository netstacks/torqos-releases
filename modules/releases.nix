# TorqOS's releases (docs/DECISIONS.md, ADR-028): where a machine updates from, and the key a
# release must be signed with. A machine's update moves it only to a commit of `url` that this key
# signed (`request system update`, torqcli's release.rs); the same key signs each release's
# checksums (docs/INSTALL.md). Not the tree's to change (settings-fence.nix): which key may decide
# what every update installs is TorqOS's own.
{ config, lib, ... }:

let
  cfg = config.torq.releases;
in
{
  options.torq.releases = {
    url = lib.mkOption {
      type = lib.types.strMatching "git\\+https://[^?]+\\?ref=[A-Za-z0-9._/-]+";
      default = "git+https://github.com/netstacks/torqos-releases?ref=stable";
      description = "The releases, as a flake reference: a git repository over HTTPS, and the branch machines follow.";
    };
    signers = lib.mkOption {
      type = lib.types.path;
      default = ../release/allowed_signers;
      description = "The keys a release may be signed with (OpenSSH's allowed-signers format, principal `torqos-release`).";
    };
  };

  config.environment.etc = {
    "torq/releases.json".text = builtins.toJSON { inherit (cfg) url; };
    "torq/release-signers".source = cfg.signers;
  };
}
