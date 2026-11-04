# TorqOS's shell: zsh with oh-my-zsh, for every user (docs/ROADMAP.md, M1.5 "terminal look").
#
# Every value here is a default (mkDefault): what the configuration tree sets wins, so the theme, the
# plugins or the shell itself are one `set` away (`set programs zsh ohMyZsh theme agnoster`). A
# person's own ~/.zshrc is theirs, as anything in their home is. torq.shell.enable = false leaves
# the shell to NixOS's defaults.
{ config, lib, pkgs, ... }:

let
  cfg = config.torq.shell;
  # oh-my-zsh's custom folder: the TorqOS theme (themes/torq.zsh-theme).
  custom = pkgs.runCommand "torq-zsh-custom" { } ''
    mkdir -p $out/themes
    cp ${./shell/torq.zsh-theme} $out/themes/torq.zsh-theme
  '';
  # The palette (branding/README.md, torqos.org) as the nearest of 256 terminal colours, for what
  # takes one style for every terminal; the prompt picks exact colours itself (shell/torq.zsh-theme).
  colour = {
    torque = "208"; # #FF7A1A
    fg = "255"; # #ECEDEF
    steel = "246"; # #8A93A3
    slate = "241"; # #5B6270
    green = "115"; # #7EE2A8
    alarm = "203"; # not the palette's: a warning, as the prompt's
    line = "236"; # #22252B, torqos.org's rules and panels
  };
in
{
  options.torq.shell.enable = lib.mkOption {
    type = lib.types.bool;
    default = true;
    description = "TorqOS's shell: zsh with oh-my-zsh, the TorqOS prompt, autosuggestions and syntax highlighting, for every user.";
  };

  config = lib.mkIf cfg.enable {
    programs.zsh = {
      enable = true;
      enableCompletion = lib.mkDefault true;
      autosuggestions.enable = lib.mkDefault true;
      # What would be typed next, in Slate: there, and quiet (the palette, branding/README.md).
      autosuggestions.highlightStyle = lib.mkDefault "fg=${colour.slate}";
      syntaxHighlighting.enable = lib.mkDefault true;
      # The command line in TorqOS's colours, as the prompt is: a command that exists in Signal
      # green, one that does not in red, the shell's own words in Torque, the rest in Steel/Slate.
      syntaxHighlighting.styles = lib.mkDefault {
        command = "fg=${colour.green}";
        builtin = "fg=${colour.green}";
        alias = "fg=${colour.green}";
        function = "fg=${colour.green}";
        precommand = "fg=${colour.green},underline";
        unknown-token = "fg=${colour.alarm}";
        reserved-word = "fg=${colour.torque}";
        globbing = "fg=${colour.torque}";
        history-expansion = "fg=${colour.torque}";
        single-quoted-argument = "fg=${colour.steel}";
        double-quoted-argument = "fg=${colour.steel}";
        dollar-quoted-argument = "fg=${colour.steel}";
        comment = "fg=${colour.slate}";
      };
      histSize = lib.mkDefault 50000;
      # A new user (no ~/.zshrc yet) gets the TorqOS shell at once, not zsh's set-up questions.
      shellInit = "zsh-newuser-install() { :; }";
      ohMyZsh = {
        enable = lib.mkDefault true;
        theme = lib.mkDefault "torq";
        custom = lib.mkDefault "${custom}";
        # git: aliases and the branch in the prompt; sudo: Esc twice puts sudo in front; z: jump to
        # a folder by part of its name; extract: unpack any archive; fzf: Ctrl-R searches history.
        plugins = lib.mkDefault [ "git" "sudo" "z" "extract" "colored-man-pages" "fzf" ];
      };
    };
    # Above NixOS's own default (bash, mkDefault), below anything set in the tree.
    users.defaultUserShell = lib.mkOverride 900 pkgs.zsh;
    environment.systemPackages = [ pkgs.fzf ];
    # fzf (Ctrl-R, Ctrl-T) in the same colours: matches and the pointer in Torque.
    environment.variables.FZF_DEFAULT_OPTS = lib.mkDefault (lib.concatStringsSep "," [
      "--color=fg:${colour.fg}" "hl:${colour.torque}" "fg+:${colour.fg}" "bg+:${colour.line}" "hl+:${colour.torque}"
      "info:${colour.steel}" "prompt:${colour.torque}" "pointer:${colour.torque}" "marker:${colour.green}"
      "spinner:${colour.torque}" "header:${colour.steel}" "border:${colour.slate}"
    ]);
  };
}
