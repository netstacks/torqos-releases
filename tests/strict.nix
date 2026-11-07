# NixOS VM test: strict mode and `torq break-glass` (ADR-035). An administrator's sudo runs TorqOS's
# own commands and nothing else; a root shell is break-glass, which leaves its ledger entry first,
# and pushes it to the remote before the shell opens. The agent has no way to it.
{ pkgs, self }:

let
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
in
# What strict mode refuses to be built with: read from the assertions, not from a failed evaluation
# (which any mistake here would also be).
assert
  let
    failed = extra: (import "${pkgs.path}/nixos/lib/eval-config.nix" {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [ self.nixosModules.default { torq.user = "dev"; torq.security.strict.enable = true; } extra ];
    }).config.assertions;
    says = text: l: pkgs.lib.any (a: !a.assertion && pkgs.lib.hasInfix text a.message) l;
  in
  !(says "without break-glass" (failed { }))
  && says "without break-glass" (failed { users.users.dev.extraGroups = [ "docker" ]; })
  && says "without break-glass" (failed { users.groups.disk.members = [ "dev" ]; })
  && says "without break-glass" (failed { users.users.dev.group = "docker"; })
  && says "not with dev mode" (failed { torq.dev.enable = true; });
pkgs.testers.runNixOSTest {
  name = "torq-strict";

  nodes.gitserver = { pkgs, ... }: {
    services.openssh.enable = true;
    users.users.git = { isNormalUser = true; shell = pkgs.bash; };
    environment.systemPackages = [ pkgs.git ];
  };

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    torq.broker.rebuildCommand = "true";
    torq.remote.url = "ssh://git@gitserver/home/git/config.git";
    torq.security.strict.enable = true;
    security.polkit.enable = true;
    environment.systemPackages = [ pkgs.jq pkgs.tmux pkgs.git ];
  };

  testScript = ''
    import json, shlex
    start_all()
    gitserver.wait_for_unit("sshd.service")
    gitserver.succeed("su - git -c 'git init -q --bare config.git && mkdir -p ~/.ssh && chmod 700 ~/.ssh'")
    machine.wait_for_unit("multi-user.target")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")
    key = json.loads(machine.succeed("torq-broker --json remote-key"))["public_key"]
    gitserver.succeed(f"echo {shlex.quote(key)} > /home/git/.ssh/authorized_keys && chown git /home/git/.ssh/authorized_keys && chmod 600 /home/git/.ssh/authorized_keys")
    machine.succeed("torq-broker push")
    pw = "Torq-sock-2210"

    def sudo(cmd):
        return machine.execute(f"su - dev -c {shlex.quote(f'printf %s {pw} | sudo -S -k -p \"\" {cmd}')} 2>&1")

    def last():
        return json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))

    def count():
        return machine.succeed("torq-broker log --json --brief | wc -l").strip()

    def as_dev(cmd):
        return "su - dev -c " + shlex.quote(cmd)

    def term(name, cmd):
        machine.succeed(as_dev(f"tmux new-session -d -s {name} -x 150 -y 40 {shlex.quote(cmd)}"))

    def keys(name, *text):
        machine.succeed(as_dev(f"tmux send-keys -t {name} {' '.join(shlex.quote(t) for t in text)} Enter"))

    def shown(name, text, timeout=60):
        machine.wait_until_succeeds(as_dev(f"tmux capture-pane -p -t {name}") + f" | grep -qF {shlex.quote(text)}", timeout=timeout)

    def on_screen(name):
        return machine.succeed(as_dev(f"tmux capture-pane -p -t {name}"))

    def ended(rc, code):
        machine.wait_until_succeeds(f"test \"$(cat {rc})\" = {code}", timeout=30)

    with subtest("sudo grants break-glass and the agent's launcher, and nothing else"):
        # What sudo grants, from sudo itself: a refusal alone would pass for any reason at all.
        code, out = sudo("-l")
        assert code == 0, out
        granted = out.split("may run the following commands")[1]
        assert granted.count("/nix/store/") == 2, out
        assert "torq-break-glass" in granted and "torq-agent-run" in granted, out
        for cmd in ["bash -c id", "true", "cat /etc/shadow", "su -", "env bash", "torq-broker verify", "torq-broker password --user root --hash-file /etc/shadow", "/run/current-system/sw/bin/torq-broker log"]:
            code, out = sudo(cmd)
            assert code != 0 and "not allowed" in out, (cmd, out)
        # What a person needs needs no sudo, the agent's program still starts, and polkit's
        # administrator is root.
        machine.succeed(as_dev("torqcli show system ledger"))
        assert machine.succeed(as_dev("torq agent run -- id -un")).strip() == "torq-agent"
        machine.succeed("grep -Rq 'unix-user:0' /etc/polkit-1/rules.d")
        machine.fail("grep -R wheel /etc/polkit-1/rules.d")
        # The agent's account has no way at all.
        machine.fail("su -s /bin/sh torq-agent -c 'sudo -n true'")
        out = machine.fail("su -s /bin/sh torq-agent -c 'torq break-glass anything' 2>&1")
        assert "no root shell" in out, out

    with subtest("no reason, no shell"):
        out = machine.fail(as_dev("torq break-glass") + " 2>&1")
        assert "usage" in out, out
        n = count()
        code, out = sudo("$(readlink -f /run/current-system/sw/bin/torq-break-glass)")
        assert code != 0 and "say why" in out, out
        assert count() == n

    with subtest("no ledger, no shell: an entry that cannot be written opens nothing (the audit of M3, M6)"):
        machine.succeed("chattr +i /var/lib/torq/state/ledger.jsonl")
        n = count()
        code, out = sudo("$(readlink -f /run/current-system/sw/bin/torq-break-glass) the ledger is stuck")
        machine.succeed("chattr -i /var/lib/torq/state/ledger.jsonl")
        assert code != 0 and "Not on the record: no shell" in out and "Root shell" not in out, out
        assert count() == n

    with subtest("break-glass: its entry first, and on the remote, before the shell opens"):
        n = count()
        term("g", "torq break-glass the disk is full; sleep 600")
        shown("g", "password for dev")
        # Nothing yet: the password comes first.
        assert count() == n
        keys("g", pw)
        shown("g", "Root shell, on the record")
        e = last()
        assert e["kind"] == "break_glass" and e["approved_by"] == "dev" and e["reason"] == "the disk is full", e
        got = gitserver.succeed("git -c safe.directory='*' -C /home/git/config.git cat-file blob refs/heads/torq-ledger:ledger.jsonl")
        assert json.loads(got.splitlines()[-1])["kind"] == "break_glass", "on the remote"
        # And it is a root shell.
        keys("g", "id -u > /tmp/who")
        machine.wait_until_succeeds("test \"$(cat /tmp/who)\" = 0", timeout=30)
        keys("g", "exit")
        out = machine.succeed(as_dev("torqcli show system ledger"))
        assert "Root shell opened: the disk is full" in out, out

    with subtest("its password every time: the shell before does not stand for this one"):
        term("k", "torq break-glass one; torq break-glass two; sleep 600")
        shown("k", "password for dev")
        keys("k", pw)
        shown("k", "Root shell, on the record")
        keys("k", "exit")
        machine.wait_until_succeeds(as_dev("tmux capture-pane -p -t k") + " | grep -c 'password for dev' | grep -qx 2", timeout=30)
        assert last()["reason"] == "one", "the second is not recorded until its password is given"
        keys("k", pw)
        machine.wait_until_succeeds("torq-broker log --json --brief | tail -1 | grep -q '\"two\"'", timeout=30)
        shown("k", "Root shell, on the record")
        keys("k", "exit")

    with subtest("a reason may begin with a dash, and is the record's text"):
        term("d", "torq break-glass --rebuild-cmd=x; sleep 600")
        shown("d", "password for dev")
        keys("d", pw)
        shown("d", "Root shell, on the record")
        assert last()["reason"] == "--rebuild-cmd=x", last()
        keys("d", "exit")

    with subtest("the remote out of reach: the entry stands, and the shell opens only on yes"):
        gitserver.succeed("systemctl stop sshd")
        for name, answer, code in [("h", "no", "rc=1"), ("e", None, "rc=1")]:
            redirect = " < /dev/null" if answer is None else ""
            term(name, f"torq break-glass network down {name}{redirect}; echo rc=$? > /tmp/rc-{name}; sleep 600")
            shown(name, "password for dev")
            keys(name, pw)
            shown(name, "Open the root shell all the same", timeout=120) if answer else None
            if answer:
                keys(name, answer)
            ended(f"/tmp/rc-{name}", code)
            assert last()["reason"] == f"network down {name}", last()
            assert "Root shell, on the record" not in on_screen(name)
        term("y", "torq break-glass network down y; sleep 600")
        shown("y", "password for dev")
        keys("y", pw)
        shown("y", "Open the root shell all the same", timeout=120)
        keys("y", "yes")
        shown("y", "Root shell, on the record")
        keys("y", "exit")
        gitserver.succeed("systemctl start sshd")
        gitserver.wait_for_unit("sshd.service")
        machine.succeed("torq-broker push")
        got = gitserver.succeed("git -c safe.directory='*' -C /home/git/config.git cat-file blob refs/heads/torq-ledger:ledger.jsonl")
        assert "network down h" in got and "network down y" in got
  '';
}
