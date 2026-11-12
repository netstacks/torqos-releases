# NixOS VM test: a person's SSH tunnels on TorqOS Desktop (ADR-050). Each claim of the ADR checked against the
# real unit, ssh and sshd, between two machines:
#   - a tunnel from the person's file comes up, carries traffic and DNS from the far end, and comes back by
#     itself after the link drops (plain ssh under systemd: the owner's "if it works");
#   - its forwards listen on 127.0.0.1 only;
#   - a host whose key is not already known does not connect, and no key is added to known_hosts;
#   - nothing in the file becomes an ssh option: a destination like `-oProxyCommand=…` or a `proxycommand`
#     setting is refused, runs nothing, and is not tried again;
#   - one wrong tunnel in the file stops only itself, and the panel says what is wrong and why one is down;
#   - the agent's account can neither stop nor start the person's tunnel, nor read their file;
#   - the applet is in COSMIC's applet list, not in the default panel.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-tunnels";
  # The desktop sets nixpkgs.overlays (COSMIC from its own nixpkgs), so the nodes build their own package set.
  node.pkgsReadOnly = false;

  nodes.desktop = { lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "alice";
    torq.desktop.enable = true;
    nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    virtualisation.memorySize = 4096;
    # The test's stand-in for a login: alice's user manager runs without one. (The product does not linger:
    # ADR-050, settled 3.)
    users.users.alice.linger = true;
    environment.systemPackages = [ pkgs.curl ];
  };

  # The far end: a plain machine with sshd, and a web page only its own loopback serves.
  nodes.far = { pkgs, ... }: {
    services.openssh.enable = true;
    services.openssh.settings.PasswordAuthentication = false;
    users.users.alice.isNormalUser = true;
    systemd.services.web = {
      wantedBy = [ "multi-user.target" ];
      script = "mkdir -p /srv && echo far-side > /srv/index.html && exec ${pkgs.python3}/bin/python3 -m http.server --bind 127.0.0.1 --directory /srv 8000";
    };
  };

  testScript = ''
    import shlex
    start_all()
    desktop.wait_for_unit("multi-user.target")
    far.wait_for_unit("sshd.service")
    far.wait_for_unit("web.service")
    uid = desktop.succeed("id -u alice").strip()
    desktop.wait_for_unit(f"user@{uid}.service")
    bus = f"XDG_RUNTIME_DIR=/run/user/{uid} DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus"

    def alice(cmd):
        return f"runuser -u alice -- env {bus} HOME=/home/alice sh -c {shlex.quote(cmd)}"

    def unit(name, prop):
        return desktop.succeed(alice(f"systemctl --user show -P {prop} torq-tunnel@{name}.service")).strip()

    def write_file(text):
        desktop.succeed(alice(f"mkdir -p ~/.config/torq && printf %s {shlex.quote(text)} > ~/.config/torq/tunnels.toml"))

    socks = "curl -sf --max-time 10 --socks5-hostname 127.0.0.1:1080 http://localhost:8000/ | grep -qx far-side"

    with subtest("alice's key on the far end, and its host key known by name only"):
        desktop.succeed(alice("mkdir -p -m 700 ~/.ssh && ssh-keygen -q -t ed25519 -N ''' -f ~/.ssh/id_ed25519"))
        key = desktop.succeed("cat /home/alice/.ssh/id_ed25519.pub").strip()
        far.succeed(f"install -d -m 700 -o alice /home/alice/.ssh && echo {shlex.quote(key)} > /home/alice/.ssh/authorized_keys && chown alice /home/alice/.ssh/authorized_keys")
        hostkey = far.succeed("cat /etc/ssh/ssh_host_ed25519_key.pub").strip()
        desktop.succeed(alice(f"echo far {shlex.quote(hostkey)} > ~/.ssh/known_hosts"))
        farip = far.succeed("ip -4 -o addr show eth1 | awk '{print $4}' | cut -d/ -f1").strip()
        # Before any tunnel ran: ssh must add nothing to it, not even a known server's other keys.
        known = desktop.succeed("sha256sum /home/alice/.ssh/known_hosts")

    write_file(f"""
    [far]
    destination = "alice@far"
    forwards = ["socks 1080", "local 8001 localhost:8000"]

    [stranger]
    destination = "alice@{farip}"
    forwards = ["socks 1081"]
    """)

    def status(name):
        # torq-tunnel status: name, state, at-login, why (the panel's own words, from the same library).
        for line in desktop.succeed(alice("torq-tunnel status")).splitlines():
            f = line.split("\t")
            if f[0] == name:
                return f
        raise Exception(f"no {name} in torq-tunnel status")

    with subtest("a tunnel from the file comes up and carries traffic, its DNS from the far end"):
        desktop.succeed(alice("torq-tunnel check | grep -q '^far '"))
        assert status("far")[1] == "off"
        desktop.succeed(alice("systemctl --user start torq-tunnel@far.service"))
        desktop.wait_until_succeeds(socks, timeout=60)
        desktop.succeed("curl -sf --max-time 10 http://127.0.0.1:8001/ | grep -qx far-side")
        # localhost:8000 is the far end's own loopback: nothing here serves it.
        desktop.fail("curl -sf --max-time 5 http://localhost:8000/")
        assert status("far")[1] == "up", status("far")

    with subtest("its forwards listen on 127.0.0.1 only"):
        listen = desktop.succeed("ss -ltnH '( sport = :1080 or sport = :8001 )' | awk '{print $4}'").split()
        assert listen and all(a.startswith("127.0.0.1:") for a in listen), listen

    with subtest("it comes back by itself after the link drops"):
        before = int(unit("far", "NRestarts"))
        far.block()
        # ssh notices within 45 s (ServerAlive), exits, and systemd starts it again, and again, while the link is down.
        desktop.wait_until_succeeds(f"test $({alice('systemctl --user show -P NRestarts torq-tunnel@far.service')}) -gt {before}", timeout=120)
        desktop.fail(socks)
        far.unblock()
        desktop.wait_until_succeeds(socks, timeout=180)
        assert unit("far", "ActiveState") == "active"

    with subtest("a host whose key is not known does not connect, its key is not added, and the panel says why"):
        desktop.succeed(alice("systemctl --user start torq-tunnel@stranger.service"))
        desktop.wait_until_succeeds(alice("journalctl --user -u torq-tunnel@stranger.service --no-pager | grep -q 'Host key verification failed'"), timeout=60)
        desktop.fail("ss -ltnH '( sport = :1081 )' | grep -q .")
        assert desktop.succeed("sha256sum /home/alice/.ssh/known_hosts") == known
        # Tried again (it is not the file's fault), never connected; and the panel's line is ssh's reason in
        # its words, not the user manager's own message about the unit.
        why = f"The key of {farip} is not known. Run `ssh alice@{farip}` once in a terminal and check it."
        # Between attempts (an attempt itself shows as connecting for a moment).
        desktop.wait_until_succeeds(alice(f"torq-tunnel status | awk -F'\\t' '$1 == \"stranger\" && $2 == \"retrying\" {{print $4}}' | grep -qxF {shlex.quote(why)}"), timeout=60)
        desktop.succeed(alice("systemctl --user stop torq-tunnel@stranger.service"))

    with subtest("the agent's account can neither stop nor start alice's tunnel, nor read her file"):
        # It is a working account: each refusal below is the tunnel's, not runuser's.
        desktop.succeed("runuser -u torq-agent -- true")
        desktop.fail(f"runuser -u torq-agent -- env {bus} systemctl --user stop torq-tunnel@far.service")
        desktop.fail("runuser -u torq-agent -- systemctl --user --machine=alice@ stop torq-tunnel@far.service")
        desktop.fail("runuser -u torq-agent -- cat /home/alice/.config/torq/tunnels.toml")
        desktop.fail("runuser -u torq-agent -- ls /home/alice/.ssh")
        assert unit("far", "ActiveState") == "active"
        desktop.succeed(socks)
        desktop.succeed(alice("systemctl --user stop torq-tunnel@far.service"))
        desktop.fail(f"runuser -u torq-agent -- env {bus} systemctl --user start torq-tunnel@far.service")
        desktop.fail("runuser -u torq-agent -- systemctl --user --machine=alice@ start torq-tunnel@far.service")
        assert unit("far", "ActiveState") == "inactive"
        desktop.succeed(alice("systemctl --user start torq-tunnel@far.service"))
        desktop.wait_until_succeeds(socks, timeout=60)

    with subtest("nothing in the file becomes an ssh option: refused, runs nothing, not tried again"):
        bad = "/tmp/badconf"
        desktop.succeed(alice(f"mkdir -p {bad}/torq && printf '%s\\n' '[x]' 'destination = \"-oProxyCommand=touch /tmp/pwned\"' 'forwards = [\"socks 1090\"]' > {bad}/torq/tunnels.toml"))
        desktop.succeed(alice(f"XDG_CONFIG_HOME={bad} torq-tunnel run x 2>&1 | grep -q 'is not a destination'"))
        desktop.succeed(alice(f"XDG_CONFIG_HOME={bad} torq-tunnel run x; test $? -eq 78"))
        # And an ssh option as a setting of its own, through the unit, beside a tunnel that is right.
        write_file('[far]\ndestination = "alice@far"\nforwards = ["socks 1080"]\n\n[odd]\ndestination = "alice@far"\nforwards = ["socks 1082"]\nproxycommand = "touch /tmp/pwned2"\n')
        desktop.succeed(alice("systemctl --user start torq-tunnel@odd.service || true"))
        desktop.wait_until_succeeds(f"test \"$({alice('systemctl --user show -P ActiveState torq-tunnel@odd.service')})\" = failed", timeout=30)
        desktop.sleep(10)
        assert unit("odd", "ActiveState") == "failed", "a wrong tunnel is not tried again"
        assert unit("odd", "NRestarts") == "0"
        s = status("odd")
        assert s[1] == "wrong" and "unknown field `proxycommand`" in s[3], s
        desktop.fail("test -e /tmp/pwned || test -e /tmp/pwned2")
        far.fail("test -e /tmp/pwned || test -e /tmp/pwned2")

    with subtest("one tunnel wrong does not stop the others: a restart of the right one still connects"):
        before = int(unit("far", "NRestarts"))
        desktop.succeed(alice("systemctl --user kill --signal=SIGKILL torq-tunnel@far.service"))
        desktop.wait_until_succeeds(f"test $({alice('systemctl --user show -P NRestarts torq-tunnel@far.service')}) -gt {before}", timeout=60)
        desktop.wait_until_succeeds(socks, timeout=60)
        assert unit("far", "ActiveState") == "active"

    with subtest("the applet is in COSMIC's applet list, not in the default panel"):
        desktop.succeed("grep -qx X-CosmicApplet=true /run/current-system/sw/share/applications/org.torqos.Tunnels.desktop")
        desktop.succeed("test -x /run/current-system/sw/bin/torq-tunnels")
        desktop.succeed("test -e /etc/systemd/user/torq-tunnel@.service")
        # The default panel is this folder (the TorqOS applet is in it): the tunnels applet is not.
        desktop.succeed("grep -rq org.torqos.Applet /run/current-system/sw/share/cosmic/com.system76.CosmicPanel.Panel/")
        desktop.fail("grep -rq org.torqos.Tunnels /run/current-system/sw/share/cosmic/")
  '';
}
