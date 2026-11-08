# NixOS VM test: the agent's account and what it may propose (ADR-041). Each claim is checked
# against the broker itself, over its socket, as the agent's account: what its list allows is
# proposed; everything else is refused, and the refusal is in the ledger.
{ pkgs, self }:

let
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
in
pkgs.testers.runNixOSTest {
  name = "torq-agent";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    users.users.sam = { isNormalUser = true; hashedPasswordFile = lib.mkForce null; hashedPassword = "!"; };
    # The owner lets the agent propose nginx's settings, and nothing else of the system (not ssh, which
    # could cut the machine off, ADR-042, 6: kept out of this test's plain apply).
    torq.agent.mayPropose.settings = [ "services.nginx" ];
    torq.broker.rebuildCommand = "true";
    environment.systemPackages = [ pkgs.socat pkgs.jq pkgs.tmux ];
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("torq-broker.socket")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix")
    machine.succeed(f"mkdir -p {repo}/nixos && echo '{{\"nginx\": {{\"enable\": false}}, \"openssh\": {{\"enable\": false}}}}' > {repo}/nixos/services.json")
    machine.succeed("torq-broker init")

    def ask(user, request):
        line = json.dumps(request)
        cmd = f"printf '%s\\n' {shlex.quote(line)} | socat -t 30 - UNIX-CONNECT:/run/torq/broker.sock"
        raw = machine.succeed(f"su -s /bin/sh {user} -c {shlex.quote(cmd)}")
        code, out = None, ""
        for l in raw.splitlines():
            f = json.loads(l)
            out += f.get("out", "")
            code = f.get("exit", code)
        return code, (json.loads(out) if out.strip() else None)

    def head():
        return machine.succeed(f"git -C {repo} rev-parse HEAD").strip()

    def propose(diff, reason):
        return ask("torq-agent", {"args": ["--json", "propose", f"--base={head()}", f"--reason={reason}"], "diff": diff})

    def last():
        return json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))

    def new_file(path, body):
        return f"--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+{body}\n"

    def services(after):
        return ("--- a/nixos/services.json\n+++ b/nixos/services.json\n@@ -1 +1 @@\n"
                "-{\"nginx\": {\"enable\": false}, \"openssh\": {\"enable\": false}}\n+" + after + "\n")

    with subtest("the account: its own, no password, no login, no groups, its home its own"):
        machine.succeed("getent passwd torq-agent | grep -q ':/var/lib/torq-agent:.*nologin$'")
        machine.succeed("test \"$(id -Gn torq-agent)\" = torq-agent")
        machine.succeed("passwd -S torq-agent | grep -q ' L '")
        machine.succeed("test \"$(stat -c %a:%U /var/lib/torq-agent)\" = 700:torq-agent")
        machine.fail("su -s /bin/sh torq-agent -c 'sudo -n true'")
        machine.fail("su - torq-agent -c true")  # its login shell is nologin
        # The owner's home is not the agent's to read.
        machine.fail("su -s /bin/sh torq-agent -c 'ls /home/dev'")
        machine.succeed("jq -e '.agent.users == [\"dev\"] and .agent.settings == [\"services.nginx\"]' /etc/torq/broker.json")

    with subtest("what its list allows, it proposes"):
        code, v = propose(new_file("users/dev/packages.json", "[\"htop\"]"), "htop for dev")
        assert code == 0 and v["kind"] == "proposal" and v["actor"] == "agent" and v["data"]["caller"] == "torq-agent", (code, v)
        code, v = propose(services("{\"nginx\": {\"enable\": true}, \"openssh\": {\"enable\": false}}"), "nginx on")
        assert code == 0 and v["kind"] == "proposal", (code, v)

    with subtest("everything else is refused, and the refusal is ledgered"):
        for diff, why in [
            (services("{\"nginx\": {\"enable\": false}, \"openssh\": {\"enable\": true}}"), "services.openssh.enable"),
            (services("{\"nginx\": {\"enable\": true}, \"openssh\": {\"enable\": true}}"), "services.openssh.enable"),
            (new_file("users/sam/packages.json", "[\"htop\"]"), "packages only for dev"),
            (new_file("nixos/torq.json", "{\"agent\": {\"mayPropose\": {\"settings\": [\"nix\"]}}}"), "torq.agent"),
            (new_file("extra.nix", "{ }"), "not a setting"),
            (new_file("config-files.json", "{\"\": \"extra.nix\"}"), "not a setting"),
        ]:
            before = head()
            code, v = propose(diff, "try")
            assert code == 1 and why in v["error"], (why, code, v)
            e = last()
            assert e["kind"] == "refused" and e["actor"] == "agent" and why in e["data"]["why"] and "diff" not in e["data"], (why, e)
            assert head() == before
        # flake.nix: no setting either.
        code, v = propose("--- a/flake.nix\n+++ b/flake.nix\n@@ -0,0 +1 @@\n+{ }\n", "the flake")
        assert code == 1, (code, v)
        machine.succeed("torq-broker verify")

    with subtest("the agent cannot apply what it proposed; a person can"):
        p = [e for e in map(json.loads, machine.succeed("torq-broker log --json --brief").splitlines()) if e["kind"] == "proposal" and e["reason"] == "htop for dev"][0]
        code, v = ask("torq-agent", {"args": ["--json", "apply", str(p["seq"])]})
        assert code == 1 and "a person applies" in v["error"], (code, v)
        # (No session at the broker: tainted, so said so explicitly — ADR-042, amended.)
        code, v = ask("dev", {"args": ["--json", "apply", str(p["seq"]), "--confirm-tainted"]})
        assert code == 0 and v["approved_by"] == "dev", (code, v)

    with subtest("a person sees the agent's proposals and applies one, at the terminal"):
        # Proposed against the configuration as it is now (the one before was prepared against an
        # older one, and the broker would refuse it: the configuration has moved on since).
        code, v = propose(services("{\"nginx\": {\"enable\": true}, \"openssh\": {\"enable\": false}}"), "nginx on, now")
        assert code == 0, (code, v)
        out = machine.succeed("su - dev -c 'torqcli show system proposals'")
        assert "nginx on" in out and "agent (torq-agent)" in out and "htop for dev" not in out, out
        ssh = v["seq"]
        # Not a proposal, or one already applied: said, nothing done.
        out = machine.fail("su - dev -c 'torqcli request system apply 1' 2>&1")
        assert "not a proposal waiting" in out, out
        # Without a terminal, nobody to ask: not applied.
        machine.fail(f"su - dev -c 'torqcli request system apply {ssh}' < /dev/null")
        machine.succeed(f"git -C {repo} show HEAD:nixos/services.json | grep -q '\"nginx\": {{\"enable\": false}}'")
        # At a terminal: the diff shown, the question, then dev's password (a system change).
        machine.succeed(f"su - dev -c \"tmux new-session -d -s a -x 160 -y 50 'torqcli request system apply {ssh}; sleep 600'\"")
        pane = lambda: machine.succeed("su - dev -c 'tmux capture-pane -p -t a'")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t a' | grep -q 'Apply?'", timeout=60)
        assert "Proposal #" in pane() and "by torq-agent" in pane() and "nginx" in pane() and "This changes the system" in pane(), pane()
        machine.succeed("su - dev -c 'tmux send-keys -t a y Enter'")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t a' | grep -q 'Password for dev:'", timeout=60)
        machine.succeed("su - dev -c 'tmux send-keys -t a Torq-sock-2210 Enter'")
        try:
            machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t a' | grep -q 'applied proposal'", timeout=60)
        except Exception:
            print(pane())
            raise
        machine.succeed(f"git -C {repo} show HEAD:nixos/services.json | grep -q '\"nginx\": {{\"enable\": true}}'")
        e = last()
        assert e["kind"] == "applied" and e["approved_by"] == "dev" and e["data"]["proposal"] == ssh, e
        out = machine.succeed("su - dev -c 'torqcli show system proposals'")
        assert "nginx on, now" not in out, out
  '';
}
