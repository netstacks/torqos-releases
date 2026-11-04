# NixOS VM test: the TorqOS image, from its first boot to its first update (docs/INSTALL.md,
# ADR-028). The image boots to setup on its console, with nobody to log in as and nothing
# listening; setup, answered on the console, records change #1 and switches to the machine it
# described; its owner's `request system update` moves it to the releases, taking only a commit the
# release key signed.
#
# Two machines: `releases`, a releases repository over HTTPS (git's own smart HTTP behind nginx),
# whose `stable` is this TorqOS signed with a test release key, and a `forged` commit signed with
# another key; and `machine`, the image's system, updating from it. The VM cannot build systems
# (no network, no build closure): setup's build is a prebuilt specialisation of the machine it will
# describe, as trust-loop.nix's commit confirmed is. Everything else — the console, setup, the
# broker, the ledger, the lock, the signature check — is real.
{ pkgs, self }:

let
  inherit (pkgs) lib;

  # The releases repository: this TorqOS, one commit signed with the test release key on `stable`;
  # then a commit signed with another key, on `forged`.
  releasesRepo = pkgs.runCommand "torq-test-releases" { nativeBuildInputs = [ pkgs.git pkgs.openssh ]; } ''
    export HOME=$PWD GIT_CONFIG_NOSYSTEM=1
    export GIT_AUTHOR_DATE=@1790000000 GIT_COMMITTER_DATE=@1790000000
    ssh-keygen -q -t ed25519 -N "" -C release -f release
    ssh-keygen -q -t ed25519 -N "" -C forger -f forger
    mkdir $out
    echo "torqos-release namespaces=\"git,file\" $(cut -d' ' -f1,2 release.pub)" > $out/allowed_signers
    cp -r ${self} work && chmod -R u+w work && cd work
    git="git -c user.name=release -c user.email=release@torqos.invalid -c gpg.format=ssh"
    git init -q -b stable
    git add -A
    $git -c user.signingkey=../release commit -q -S -m "TorqOS test release"
    git checkout -q -b forged
    echo "forged" > FORGED
    git add FORGED
    $git -c user.signingkey=../forger commit -q -S -m "TorqOS, signed by someone else"
    git clone -q --bare . $out/torqos-releases.git
  '';

  # The releases server's certificate: its own authority, which `machine` trusts.
  certs = pkgs.runCommand "torq-test-releases-cert" { nativeBuildInputs = [ pkgs.openssl ]; } ''
    mkdir $out
    openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 3650 \
      -subj /CN=releases -addext subjectAltName=DNS:releases \
      -keyout $out/releases.key -out $out/releases.crt 2>/dev/null
  '';
in
pkgs.testers.runNixOSTest {
  name = "torq-image";

  nodes.releases = { config, ... }: {
    # git, for the test to read and move the repository's branches.
    environment.systemPackages = [ pkgs.git ];
    networking.firewall.allowedTCPPorts = [ 443 ];
    users.users.git = { isSystemUser = true; group = "git"; };
    users.groups.git = { };
    # The repository, writable by the test (moving `stable`) and owned by who serves it.
    systemd.tmpfiles.rules = [ "d /srv/git 0755 git git -" ];
    systemd.services.releases-repo = {
      wantedBy = [ "multi-user.target" ];
      before = [ "nginx.service" ];
      serviceConfig.Type = "oneshot";
      script = ''
        cp -r ${releasesRepo}/torqos-releases.git /srv/git/
        chown -R git:git /srv/git
        chmod -R u+w /srv/git
      '';
    };
    services.fcgiwrap.instances.git = {
      process = { user = "git"; group = "git"; };
      socket = { user = "nginx"; group = "nginx"; };
    };
    services.nginx = {
      enable = true;
      virtualHosts.releases = {
        onlySSL = true;
        sslCertificate = "${certs}/releases.crt";
        sslCertificateKey = "${certs}/releases.key";
        locations."/".extraConfig = ''
          include ${config.services.nginx.package}/conf/fastcgi_params;
          fastcgi_param SCRIPT_FILENAME ${pkgs.git}/libexec/git-core/git-http-backend;
          fastcgi_param GIT_PROJECT_ROOT /srv/git;
          fastcgi_param GIT_HTTP_EXPORT_ALL "";
          fastcgi_param PATH_INFO $uri;
          fastcgi_pass unix:${config.services.fcgiwrap.instances.git.socket.address};
        '';
      };
    };
  };

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.default ];
    # jq, to read the configuration's JSON (not on a TorqOS machine itself: ADR-032); the
    # specialisation setup switches to inherits it.
    environment.systemPackages = [ pkgs.jq ];
    # The image: setup on its console, its own TorqOS as the source a new machine starts from.
    torq.setup.enable = true;
    torq.setup.source = self.sourceInfo;
    torq.desktop.enable = false;
    # NixOS's test instrumentation gives root an empty password; TorqOS asserts root has none.
    users.users.root.hashedPasswordFile = lib.mkForce null;
    # The releases it updates from, and the key they must be signed with: the test's.
    torq.releases = {
      url = "git+https://releases/torqos-releases?ref=stable";
      signers = "${releasesRepo}/allowed_signers";
    };
    security.pki.certificateFiles = [ "${certs}/releases.crt" ];
    # Setup's build: the machine setup will describe, prebuilt (the VM cannot build systems); its
    # switch, into it.
    specialisation.alice.configuration = {
      torq.setup.enable = lib.mkForce false;
      torq.user = "alice";
      networking.hostName = lib.mkForce "first";
    };
    torq.broker.buildCommand = "readlink -f /run/booted-system/specialisation/alice";
    torq.broker.rebuildCommand = "/run/booted-system/specialisation/alice/bin/switch-to-configuration switch";
    torq.broker.bootCommand = "nix-env -p {profile} --set {system}";
    virtualisation.memorySize = 2048;
    nix.settings.min-free = 0;
  };

  testScript = ''
    import shlex
    start_all()
    releases.wait_for_unit("nginx.service")
    machine.wait_for_unit("multi-user.target")

    # The console's text, as it is on the screen (/dev/vcs1: no line breaks, just the cells).
    def on_console(text, timeout=120):
        try:
            machine.wait_until_succeeds(f"grep -aqF {shlex.quote(text)} /dev/vcs1", timeout=timeout)
        except Exception:
            # What the screen shows instead, what runs on it, and when the console and setup came up
            # (seconds from boot).
            print(machine.execute("fgconsole; tr -s ' ' < /dev/vcs1; echo; systemctl status --no-pager torq-setup; "
                "journalctl -b --no-pager -o short-monotonic -u torq-setup -u systemd-udev-settle -u systemd-udev-trigger -u systemd-vconsole-setup -u getty@tty1; "
                "journalctl -b --no-pager -o short-monotonic -k --grep 'Console|fbcon|drm'")[1])
            raise

    def answer(text):
        machine.send_chars(text + "\n")

    with subtest("the image boots to setup, with nobody to log in as and nothing listening"):
        on_console("Welcome to TorqOS. This machine has no configuration yet")
        on_console("Machine name [torq]:")
        # Under TorqOS's wordmark; and the machine says it is TorqOS, on NixOS (modules/branding.nix).
        on_console("set  |  commit  |  rollback")
        machine.succeed(". /etc/os-release && test \"$NAME $ID $ID_LIKE\" = 'TorqOS torqos nixos' && test \"$VERSION_ID\" = '${self.packages.${pkgs.stdenv.hostPlatform.system}.torq.version}'")
        # The base is what TorqOS runs and nothing else (ADR-032): no htop or man pages (jq this
        # test's own machine adds; curl is one of NixOS's core packages; the NixOS tools are the
        # trust-loop test's check).
        for tool in ["htop", "man"]:
            machine.fail(f"command -v {tool}")
        # A graphics driver taking the console over late can lose what was on the screen, and
        # changes its size: setup asks again. (The x86_64 VM's bochs-drm may do it by itself, or not:
        # done here, on purpose.)
        machine.succeed("printf '\\033[H\\033[2J' > /dev/tty1 && stty -F /dev/tty1 rows 30 cols 100")
        on_console("Welcome to TorqOS. This machine has no configuration yet")
        on_console("Machine name [torq]:")
        machine.succeed("systemctl is-active torq-setup")
        machine.fail("systemctl is-active getty@tty1")
        machine.succeed("passwd -S root | grep -q ' L '")
        # Listening only on the machine itself (systemd-resolved's stub), not on the network.
        out = machine.succeed("ss -Htln")
        assert all(l.split()[3].startswith(("127.", "[::1]", "[::ffff:127.")) for l in out.splitlines()), out
        machine.fail("test -s /var/lib/torq/state/ledger.jsonl")

    with subtest("setup, answered on the console, records change #1 and switches to the machine it described"):
        on_console("Machine name [torq]:")
        answer("first")
        on_console("Time zone [UTC]:")
        answer("")
        on_console("Your user name:")
        answer("alice")
        on_console("Password:")
        answer("Torq-7731-secret")
        on_console("Password again:")
        answer("Torq-7731-secret")
        on_console("Choose [1]:")
        answer("")
        on_console("logging in: not from other machines")
        answer("")
        machine.wait_until_succeeds("test -s /var/lib/torq/state/ledger.jsonl", timeout=300)
        # The switch brings the console's login prompt, which takes the console from setup.
        machine.wait_for_unit("getty@tty1.service")
        on_console("first login:", timeout=300)
        out = machine.succeed("torq-broker log")
        assert "init" in out and "TorqOS set up" in out, out
        machine.succeed("torq-broker verify")
        # The owner's password: outside the repo, readable by the system only; the account, as setup said.
        machine.succeed("test \"$(stat -c %a:%U /var/lib/torq/secrets/passwords/alice)\" = 600:root")
        # Neither the password nor its hash is anywhere in the config repo (its history included).
        found = machine.succeed("grep -rlaF -e Torq-7731-secret -e \"$(cat /var/lib/torq/secrets/passwords/alice)\" /var/lib/torq/config || true")
        assert found == "", found
        machine.succeed("id alice")
        machine.succeed("test \"$(cat /proc/sys/kernel/hostname)\" = first")
        machine.succeed("jq -e '.user == \"alice\"' /var/lib/torq/config/nixos/torq.json")
        # Its TorqOS: the image's own, in the store.
        machine.succeed("grep -q 'torq.url = \"path:/nix/store/' /var/lib/torq/config/flake.nix")
        # An SSH login at a terminal is greeted with the mark and what the machine is
        # (modules/branding.nix); one without a terminal (scp, `ssh host command`) is not.
        greeted = machine.succeed("su - alice -c \"script -qec 'env SSH_CONNECTION=\\\"10.0.2.2 1 10.0.2.15 22\\\" zsh -lic true' /dev/null\"")
        assert "osydhhdhhdhys" in greeted and "TorqOS" in greeted and "Generation" in greeted, greeted
        quiet = machine.succeed("su - alice -c \"env SSH_CONNECTION='10.0.2.2 1 10.0.2.15 22' zsh -lc 'echo done'\" < /dev/null")
        assert quiet.strip() == "done", quiet

    def as_alice(cmd, ok=True):
        run = machine.succeed if ok else machine.fail
        return run(f"su - alice -c {shlex.quote(cmd)} 2>&1")

    with subtest("the first update moves the machine to the releases, checking the release key signed them"):
        # (One `-c`: its candidate is its own, gone when it ends.)
        out = as_alice("torqcli -c 'request system update; configure; show | compare'")
        assert "the update moves it to TorqOS's releases (git+https://releases/torqos-releases?ref=stable)" in out, out
        rev = releases.succeed("git -c safe.directory='*' -C /srv/git/torqos-releases.git rev-parse stable").strip()
        assert f"TorqOS release {rev[:12]}: signed by the TorqOS release key." in out, out
        assert "+   source git+https://releases/torqos-releases?ref=stable;" in out, out

    with subtest("a release the key did not sign is refused, and nothing is loaded"):
        forged = releases.succeed("git -c safe.directory='*' -C /srv/git/torqos-releases.git rev-parse forged").strip()
        releases.succeed(f"git -c safe.directory='*' -C /srv/git/torqos-releases.git update-ref refs/heads/stable {forged}")
        out = as_alice("torqcli -c 'request system update; configure; show | compare'", ok=False)
        assert f"release {forged[:12]} is not signed by the TorqOS release key" in out, out
        assert "+   source" not in out, "nothing loaded: " + out
  '';
}
