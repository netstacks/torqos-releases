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
      syntaxHighlighting.enable = lib.mkDefault true;
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
  };
}
