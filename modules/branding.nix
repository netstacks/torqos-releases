# TorqOS's name and look wherever the system shows them (branding/README.md): what the machine says
# it is (/etc/os-release, /etc/lsb-release, the boot menu, the boot's own lines), the console's
# greeting before login and the banner over first-boot setup (the lockup: mark and wordmark), and
# fastfetch's logo. The shell's prompt
# is its own (modules/shell.nix) and not touched here.
#
# TorqOS is NixOS underneath, and says so: os-release's ID_LIKE is `nixos`, and its VERSION names
# the NixOS release it is built on. The version is TorqOS's own (torqcli's, one number for both).
{ config, lib, pkgs, ... }:

let
  version = config.torq.cli.package.version;
  nixos = config.system.nixos.release;
  # The public repository: each release's source, its images, and where to report a problem.
  repo = "https://github.com/torqosorg/torqos-releases";
  share = "${config.torq.branding.package}/share/torqos";
  # agetty reads `\` in /etc/issue as the start of an escape (`\n` the host name, `\l` the
  # terminal): the wordmark's own backslashes, doubled, are drawn as they are. The lockup — the
  # mark beside the wordmark (branding/README.md), in half blocks the console's font has.
  lockup = lib.replaceStrings [ "\\" ] [ "\\\\" ] (builtins.readFile ../branding/terminal/torqos-lockup-ansi.txt);
  # fastfetch's configuration, its logo found where TorqOS installs it (the file names a person's).
  fastfetchLogo = "~/.config/fastfetch/torqos-logo-ascii-fastfetch.txt";
  fastfetch = builtins.readFile ../branding/terminal/fastfetch-config.jsonc;
in
{
  options.torq.branding.package = lib.mkOption {
    type = lib.types.package;
    default = pkgs.callPackage ../branding/package.nix { };
    defaultText = lib.literalExpression "pkgs.callPackage ../branding/package.nix { }";
    description = "TorqOS's icons, terminal logos and fastfetch configuration (branding/).";
  };

  options.torq.shell.sshGreeting = lib.mkOption {
    type = lib.types.bool;
    default = true;
    description = "Greet an SSH login with TorqOS's mark and what this machine is (fastfetch, branding/terminal/fastfetch-config.jsonc). Only an interactive login at a terminal, never `scp` or `ssh host command`; `~/.hushlogin` turns it off for one person.";
  };

  config = lib.mkMerge [ {
    system.nixos = {
      distroName = "TorqOS";
      distroId = "torqos";
      vendorName = "TorqOS";
      vendorId = "torqos";
      # Each value replaces NixOS's (BUILD_ID, the NixOS build, and SUPPORT_END, its release's end
      # of support, stay: TorqOS is supported as long as the NixOS release under it).
      extraOSReleaseArgs = {
        PRETTY_NAME = "TorqOS ${version}";
        VERSION = "${version} (NixOS ${nixos})";
        VERSION_ID = version;
        VERSION_CODENAME = "";
        CPE_NAME = "cpe:/o:torqos:torqos:${version}";
        LOGO = "distributor-logo-torqos";
        ANSI_COLOR = "38;5;208";
        HOME_URL = repo;
        VENDOR_URL = repo;
        DOCUMENTATION_URL = "${repo}/tree/stable/docs";
        SUPPORT_URL = "${repo}/issues";
        BUG_REPORT_URL = "${repo}/issues";
        DEFAULT_HOSTNAME = "torq";
      };
      extraLSBReleaseArgs = {
        LSB_VERSION = "${version} (NixOS ${nixos})";
        DISTRIB_RELEASE = version;
        DISTRIB_CODENAME = "";
        DISTRIB_DESCRIPTION = "TorqOS ${version} (NixOS ${nixos})";
      };
    };

    # The console, before login: the lockup, what this is, and where to start.
    environment.etc.issue.text = ''

      ${lockup}
      TorqOS ${version} (NixOS ${nixos}) on \n, \l
      Log in, then run `torqcli` to see and change this machine's configuration.

    '';
    # The lockup over first-boot setup's questions, and the installer's (cli/src/setup.rs).
    environment.etc."torq/banner".source = "${share}/torqos-lockup-ansi.txt";
    # fastfetch, for whoever installs it: TorqOS's logo and what this machine is.
    environment.etc."xdg/fastfetch/config.jsonc".text =
      assert lib.assertMsg (lib.hasInfix fastfetchLogo fastfetch) "branding/terminal/fastfetch-config.jsonc no longer names ${fastfetchLogo}";
      lib.replaceStrings [ fastfetchLogo ] [ "${share}/torqos-logo-ascii-fastfetch.txt" ] fastfetch;
  } (lib.mkIf config.torq.shell.sshGreeting (
    let
      # fastfetch without its optional libraries (13 MB, not 190): what the greeting shows needs none.
      # Its program alone, copied from NixOS's build (nothing compiled here) and wrapped as NixOS's
      # package wraps it: the package's shell completions run Python, which would bring all of it
      # (ADR-032, amended), for a greeting that is never completed. The program's one reference to
      # its own package (`etc/`, where it would look for a system-wide configuration) is cut: it is
      # always given its configuration.
      ff = pkgs.fastfetch.minimal;
      fastfetch = "${pkgs.runCommand "fastfetch-greeting" { nativeBuildInputs = [ pkgs.makeWrapper pkgs.removeReferencesTo ]; } ''
        install -Dm755 ${ff}/bin/.fastfetch-wrapped $out/libexec/fastfetch
        remove-references-to -t ${ff} $out/libexec/fastfetch
        makeWrapper $out/libexec/fastfetch $out/bin/fastfetch --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath (map (p: p.out or p) ff.buildInputs)}
      ''}/bin/fastfetch";
      greet = ''
        if [ -n "$SSH_CONNECTION" ] && [ -t 0 ] && [ -t 1 ] && [ ! -e "$HOME/.hushlogin" ]; then
          ${fastfetch} --config /etc/xdg/fastfetch/config.jsonc 2>/dev/null
        fi
      '';
    in
    {
      # The login shell's own start (zsh's zprofile, bash's profile): once per login, not per shell.
      programs.zsh.loginShellInit = greet;
      programs.bash.loginShellInit = greet;
    })) ];
}
