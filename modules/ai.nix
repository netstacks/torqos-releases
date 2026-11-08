# The agent's runtime (ADR-042): doctor mode, and the model it asks. Off by default; nothing is
# downloaded and nothing runs until the owner turns it on, as a ledgered change like any other.
#
# The doctor (`torq-doctor`, agent/) runs as the agent's account (modules/agent.nix), started by a
# person with `torq doctor "<question>"` — through `torq agent run`, so in the launcher's sandbox and
# for that one question; there is no agent service. It reads and explains; it proposes nothing. With
# `torq agent propose "<ask>"` it may also propose one change (never apply it), its session's taint
# held by the broker (ADR-042, 6 and 7).
#
# The model: by default llama.cpp's `llama-server` (nixpkgs' services.llama-cpp) on this machine,
# listening on a unix socket only (no TCP port, so nothing else can take its place on 127.0.0.1),
# with a model file the owner names: a URL and its hash, fetched by Nix as a fixed-output derivation
# when the change is built, or a file already on the disk. Or a remote OpenAI-compatible endpoint,
# its key in a file outside the config repo, as passwords are.
#
# All of `torq.*` is never the agent's to propose (ADR-041): it cannot change its own model, endpoint,
# key or journal access.
{ config, lib, pkgs, ... }:

let
  cfg = config.torq.agent.doctor;
  m = config.torq.agent.model;
  local = cfg.endpoint == null;
  socket = "/run/llama-cpp/llama.sock";
  modelFile =
    if m.path != null then m.path
    else if m.url != null then pkgs.fetchurl { inherit (m) url hash; }
    else null;
  # llama-server's flags that would change where it listens, what it shows or lets be changed, or
  # which model it loads — TorqOS's own, never the owner's extra ones (a `--flag=value` by its flag).
  fixedFlags = [ "--host" "--port" "--slots" "--no-slots" "--props" "-m" "--model" "-mu" "--model-url" "-hf" "-hfr" "-hff" "-hfv" "-hffv" "--path" "--api-prefix" "--jinja" "--no-jinja" ];
  isFixed = f: let name = lib.head (lib.splitString "=" f); in builtins.elem name fixedFlags || lib.hasPrefix "--hf-" name || lib.hasPrefix "-hf" name;
  refusedFlags = lib.filter isFixed m.extraFlags;
in
{
  options.torq.agent.doctor = {
    enable = lib.mkEnableOption "doctor mode: `torq doctor \"<question>\"` — the agent reads logs, hardware and the configuration, and explains; it proposes nothing (ADR-042)";

    package = lib.mkOption {
      type = lib.types.package;
      description = "torq-agent package (set by the TorqOS flake).";
    };

    endpoint = lib.mkOption {
      type = lib.types.nullOr (lib.types.strMatching "(https://[^[:space:]]+|http://(127\\.0\\.0\\.1|localhost|\\[::1])(:[0-9]+)?(/[^[:space:]]*)?|unix:/[^[:space:]]+)");
      default = null;
      example = "https://api.anthropic.com/v1";
      description = ''
        The OpenAI-compatible endpoint the doctor asks (its base URL, ending in `/v1`). `null`, the
        default: the model on this machine (`torq.agent.model`), llama-server on a unix socket.
        Anything else is remote: what the tools return — logs, the configuration (a wifi network's key in
        it too), the ledger — leaves
        the machine, and the doctor says so each time it starts (ADR-005: indicated and ledgered).
        Plain http is accepted only to this machine.
      '';
    };

    model = lib.mkOption {
      type = lib.types.strMatching "[A-Za-z0-9._:/@-]+";
      default = "local";
      example = "claude-sonnet-4-5";
      description = "The model's name, as the endpoint knows it (llama-server serves its one model whatever it is called).";
    };

    apiKeyFile = lib.mkOption {
      type = lib.types.nullOr (lib.types.strMatching "/[^[:space:]]+");
      default = null;
      example = "/var/lib/torq-agent/model.key";
      description = ''
        A file holding the endpoint's API key, on one line, readable by `torq-agent` — never in the
        config repo (everyone on the machine reads it, and its history keeps every value): put it
        there yourself, e.g. `sudo install -o torq-agent -g torq-agent -m 600 key /var/lib/torq-agent/model.key`.
        A string, not a Nix path, so the key is never copied into the world-readable store.
      '';
    };

    journal = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        Whether the agent may read the system journal. Off by default, with any model. On, the whole
        `torq-agent` account joins `systemd-journal` — not only the doctor: every program run as the
        agent gains it, including any tool a person starts with `torq agent run` (Claude Code, which
        sends what it reads to its own remote service), and every later session of the agent. What it
        gains is read access to every service's and every account's log; the group writes, rotates and
        deletes nothing. With a remote endpoint, the doctor also sends what it reads of the journal off
        the machine. Off, the doctor's journal tool is not offered.
      '';
    };

    maxSteps = lib.mkOption {
      type = lib.types.ints.between 1 32;
      default = 8;
      description = "Rounds of tool calls before the model must answer.";
    };
  };

  options.torq.agent.model = {
    url = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf";
      description = ''
        The local model's file (GGUF), fetched by Nix when the change that names it is built — never at
        install, never by the agent — and checked against `hash`. It is then in the store, part of the
        generation: rolling back keeps it while that generation does.
      '';
    };
    hash = lib.mkOption {
      type = lib.types.nullOr (lib.types.strMatching "sha256-[A-Za-z0-9+/=]{44}");
      default = null;
      example = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
      description = "The model file's hash (SRI: `nix hash convert --to sri sha256:<hex>`).";
    };
    path = lib.mkOption {
      type = lib.types.nullOr (lib.types.strMatching "/[^[:space:]]+\\.gguf");
      default = null;
      example = "/var/lib/torq-model/model.gguf";
      description = "Instead of `url`: a model file already on this disk, outside the store and any home (not in the configuration's history; not checked).";
    };
    extraFlags = lib.mkOption {
      type = lib.types.listOf (lib.types.strMatching "[A-Za-z0-9._=:-]+");
      default = [ ];
      example = [ "--ctx-size" "16384" "--n-gpu-layers" "99" ];
      description = ''
        More of llama-server's flags (its context size, GPU layers). Never those that would change where
        it listens, what it shows, or which model it loads: --host, --port, --slots, --props, -m/--model,
        -mu/--model-url, -hf and the --hf-* flags are refused (and TorqOS's own come last).
      '';
    };
    memoryMax = lib.mkOption {
      type = lib.types.nullOr (lib.types.strMatching "[0-9]+[KMGT%]?");
      default = "75%";
      example = "24G";
      description = "The most memory llama-server may take (systemd's MemoryMax); it is also the first the kernel kills when memory runs out.";
    };
  };

  config = lib.mkMerge [
    {
      assertions = [
        {
          assertion = (m.url == null) == (m.hash == null);
          message = "torq.agent.model: url and hash go together (Nix checks the file it fetches against the hash).";
        }
        {
          assertion = !(m.url != null && m.path != null);
          message = "torq.agent.model: url or path, not both.";
        }
        {
          assertion = m.path == null || !(lib.hasPrefix "/home/" m.path || lib.hasPrefix "/nix/store/" m.path);
          message = "torq.agent.model.path: outside every home (the model's service cannot see them) and outside the store (use url and hash).";
        }
        {
          assertion = !(cfg.enable && local) || modelFile != null;
          message = "torq.agent.doctor: the local model needs its file — torq.agent.model.url and hash, or path — or set torq.agent.doctor.endpoint.";
        }
        {
          assertion = cfg.apiKeyFile == null || !(lib.hasPrefix "/nix/store/" cfg.apiKeyFile || lib.hasPrefix "/var/lib/torq/config" cfg.apiKeyFile);
          message = "torq.agent.doctor.apiKeyFile: outside the store and the config repo (both are readable by everyone).";
        }
        {
          assertion = refusedFlags == [ ];
          message = "torq.agent.model.extraFlags: ${lib.concatStringsSep " " refusedFlags} would change where llama-server listens, what it shows, or which model it loads: TorqOS sets those.";
        }
      ];
    }

    (lib.mkIf cfg.enable {
      environment.systemPackages = [ cfg.package ];
      environment.etc."torq/agent.json".text = builtins.toJSON {
        endpoint = if local then "unix:${socket}" else cfg.endpoint;
        inherit (cfg) model apiKeyFile journal maxSteps;
      };
      users.users.torq-agent.extraGroups = lib.optional cfg.journal "systemd-journal";
    })

    (lib.mkIf (cfg.enable && local) {
      services.llama-cpp = {
        enable = true;
        model = modelFile;
        # A path ending in .sock: llama-server listens there, on no port (its --port is then unused).
        host = socket;
        # The owner's flags first, TorqOS's last (llama-server takes a flag's last value): tool calls need
        # the model's own chat template (--jinja); /slots would show other requests' prompts to anyone
        # who can connect.
        extraFlags = m.extraFlags ++ [ "--jinja" "--no-slots" ];
      };
      # The socket in a directory only the service writes. Its mode lets local accounts connect (0666:
      # a umask of 0111; the service's other files are under DynamicUser's private, root-only parents).
      # No network at all — a unix socket works across network namespaces — so the model, and anything
      # it was made to do, reaches nothing and is reached by nothing but the socket. The first the
      # kernel kills when memory runs out, and bounded.
      systemd.services.llama-cpp.serviceConfig = {
        RuntimeDirectory = "llama-cpp";
        RuntimeDirectoryMode = "0755";
        UMask = "0111";
        PrivateNetwork = true;
        RestrictAddressFamilies = lib.mkForce [ "AF_UNIX" ];
        IPAddressDeny = "any";
        OOMScoreAdjust = 500;
      } // lib.optionalAttrs (m.memoryMax != null) { MemoryMax = m.memoryMax; };
    })
  ];
}
