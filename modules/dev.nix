{ config, lib, pkgs, ... }:

let
  cfg = config.torq.dev;
  # Where TorqOS comes from, as the configuration's own lock says (its `torq` input). A folder
  # outside the read-only store (`path:`) is for developing TorqOS only: whoever can write there
  # decides what root builds next (second audit, M3). Checked here, in the evaluation, so it holds
  # however the configuration says it — the tree, an attached file — and whoever builds it (audit of
  # 2026-11-01): torqcli's check at `set` is only the early word.
  dir = config.torq.packages.configDir;
  lockFile = if dir != null then dir + "/flake.lock" else null;
  lock = if lockFile != null && builtins.pathExists lockFile then lib.importJSON lockFile else null;
  torqNode = if lock == null then null else lock.nodes.${lock.nodes.root.inputs.torq or ""} or null;
  locked = if torqNode == null then { } else torqNode.locked or { };
  # Exactly one store path, as torqcli's software::store_path has it.
  inStore = p: builtins.match "/nix/store/[0123456789abcdfghijklmnpqrsvwxyz]{32}-[A-Za-z0-9+._?=-]+" p != null && !(lib.hasPrefix "." (lib.removePrefix "/nix/store/" p));
  fromFolder = (locked.type or "") == "path" && !(inStore (locked.path or ""));
in
{
  options.torq.dev.enable = lib.mkOption {
    type = lib.types.bool;
    default = false;
    description = ''
      Development mode: convenience over security while TorqOS itself is being built.
      Passwordless sudo for wheel, Claude Code + gh + tmux installed, the owner is a trusted Nix user.
      An AI coding agent working in this mode has full root and can bypass the broker.
      Never enable on a real install; turning it off is one broker commit (ADR-009).
    '';
  };

  config = lib.mkMerge [
    {
      assertions = [
        {
          assertion = !fromFolder || cfg.enable;
          message = "torq: TorqOS comes from a folder (path:${locked.path or ""}), which is for developing TorqOS only, with `torq dev enable true`: whoever can write there decides what root builds.";
        }
      ];
    }

    (lib.mkIf cfg.enable {
      warnings = [
        "torq.dev.enable is on: passwordless sudo and an AI coding agent with root. Development only."
      ];

      security.sudo.wheelNeedsPassword = false;
      nix.settings.trusted-users = lib.optional (config.torq.user != null) config.torq.user;

      # (Unfree: allowed by name, through the one rule for it, modules/unfree.nix.)
      torq.unfree.packages = [ "claude-code" ];
      # tmux: run Claude Code inside it (`tmux new -A -s claude`) so a dropped SSH connection — the
      # Mac sleeping, UTM pausing, the network restarting during a switch — does not kill the session.
      environment.systemPackages = with pkgs; [ claude-code gh tmux vim ];

      # Claude Code ships a new build most days; nixpkgs lags it by weeks, and the upstream
      # native installer is useless here (its binary is a plain dynamically-linked ELF — it
      # needs nixpkgs' autoPatchelfHook to run at all, and the Nix build sets
      # DISABLE_AUTOUPDATER anyway). So pin the version declaratively by overriding the
      # release manifest nixpkgs reads. To bump:
      #   curl -fsSL https://downloads.claude.ai/claude-code-releases/latest
      #   curl -fsSL https://downloads.claude.ai/claude-code-releases/<ver>/manifest.json \
      #     -o modules/claude-code-manifest.json
      nixpkgs.overlays = [
        (final: prev: {
          claude-code = prev.claude-code.override {
            manifest = lib.importJSON ./claude-code-manifest.json;
          };
        })
      ];
    })
  ];
}
