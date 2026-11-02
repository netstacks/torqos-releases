{ config, lib, ... }:

let
  cfg = config.torq.ai;
in
{
  options.torq.ai = {
    enable = lib.mkEnableOption "the TorqOS AI agent (later: the AI layers, ADR-019; not built yet)";

    modelEndpoint = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "http://192.168.64.1:8080";
      description = ''
        OpenAI-compatible endpoint of the model the agent uses. During development this is
        llama.cpp / Ollama running on the Mac host; on a TorqOS machine it would be local.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # Nothing runs yet: turning it on must not look as if an agent did (audit of 2026-10-31).
    assertions = [{
      assertion = false;
      message = "torq.ai.enable: the AI layers are not built yet (they come later, docs/ROADMAP.md); nothing would run.";
    }];

    # TODO(later, the AI layers): systemd.services.torq-agent — unprivileged, sandboxed (DynamicUser,
    # ProtectSystem=strict, bubblewrap), read-only MCP tools, talks to the broker over a socket.
  };
}
