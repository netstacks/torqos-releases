# NixOS VM test: `commit at` (broker/src/at.rs), with the module's own transient timers. A commit
# scheduled a minute ahead lands then, as the administrator who approved it; one cancelled does not,
# even run by hand after its cancel; one whose configuration moved on is refused on the record.
# Rebuilds are stubbed (`true`), as in the trust loop: this proves the schedule, not the build.
{ pkgs, self }:

let
  # The owner's password (crypt SHA-512, a fixed salt): the same as tests/socket.nix's.
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
in
pkgs.testers.runNixOSTest {
  name = "torq-commit-at";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    # Someone who is not an administrator: whose cancel of dev's schedule is refused.
    users.users.sam = { isNormalUser = true; hashedPasswordFile = lib.mkForce null; hashedPassword = "!"; };
    torq.broker.rebuildCommand = "true";
    environment.systemPackages = [ pkgs.socat pkgs.jq ];
    environment.etc."torq-test/settings.nix".text = "{ a = 0; b = 0; c = 0; }\n";
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("torq-broker.socket")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && cp /etc/torq-test/settings.nix {repo}/ && touch {repo}/flake.nix")
    machine.succeed("torq-broker init")
    password = "Torq-sock-2210"

    def ask(user, request):
        """One request over the socket as `user`: (exit code, the JSON answer)."""
        line = json.dumps(request)
        cmd = f"printf '%s\\n' {shlex.quote(line)} | socat -t 30 - UNIX-CONNECT:/run/torq/broker.sock"
        raw = machine.succeed(f"su -s /bin/sh {user} -c {shlex.quote(cmd)}")
        code, out = None, ""
        for l in raw.splitlines():
            f = json.loads(l)
            out += f.get("out", "")
            if "exit" in f:
                code = f["exit"]
        return code, json.loads(out) if out.strip() else None

    def head():
        return machine.succeed(f"git -C {repo} rev-parse HEAD").strip()

    def setting():
        return machine.succeed(f"git -C {repo} show HEAD:settings.nix").strip()

    def diff(old, new):
        return f"--- a/settings.nix\n+++ b/settings.nix\n@@ -1 +1 @@\n-{old}\n+{new}\n"

    def propose(old, new, reason):
        code, v = answer = ask("dev", {"args": ["--json", "propose", f"--base={head()}", f"--reason={reason}"], "diff": diff(old, new)})
        assert code == 0 and v["tier"] == "system", answer
        return v["seq"]

    def in_seconds(n):
        return machine.succeed(f"date -u -d '+{n} seconds' +%Y-%m-%dT%H:%M:%SZ").strip()

    def schedule(seq, seconds):
        code, v = ask("dev", {"args": ["--json", "schedule", str(seq), f"--at={in_seconds(seconds)}"], "password": password})
        assert code == 0 and v["kind"] == "apply_scheduled" and v["approved_by"] == "dev", (code, v)
        return v

    def ledger():
        return [json.loads(l) for l in machine.succeed("torq-broker --json log --brief").splitlines()]

    def scheduled():
        code, v = ask("dev", {"args": ["--json", "scheduled"]})
        assert code == 0, (code, v)
        return v["scheduled"]

    with subtest("a commit at is approved now, through the gate, and waits on a timer of its own"):
        p = propose("{ a = 0; b = 0; c = 0; }", "{ a = 1; b = 0; c = 0; }", "a at a time")
        # A system change: the administrator's password, as apply asks it.
        code, v = ask("dev", {"args": ["--json", "schedule", str(p), f"--at={in_seconds(60)}"]})
        assert code == 77 and v["password"], (code, v)
        # The agent schedules nothing; the timer's own command is not on the socket.
        code, v = ask("torq-agent", {"args": ["--json", "schedule", str(p), f"--at={in_seconds(60)}"]})
        assert code == 1 and "the agent proposes" in v["error"], (code, v)
        code, v = ask("dev", {"args": ["--json", "run-scheduled", "1"]})
        assert code == 1 and "not run over the socket" in v["error"], (code, v)
        s = schedule(p, 50)
        timer = s["data"]["timer"]
        machine.succeed(f"systemctl is-active torq-commit-at-{timer}.timer")
        assert [x["seq"] for x in scheduled()] == [s["seq"]], scheduled()
        assert "a = 0" in setting(), "not applied yet"
        # One at a time.
        q = propose("{ a = 0; b = 0; c = 0; }", "{ a = 0; b = 1; c = 0; }", "b too")
        code, v = ask("dev", {"args": ["--json", "schedule", str(q), f"--at={in_seconds(120)}"], "password": password})
        assert code == 1 and "one at a time" in v["error"], (code, v)
        # torqcli shows it with the configurations.
        out = machine.succeed("su - dev -c 'torqcli show system commit'")
        assert f"Scheduled: #{s['seq']}" in out and "by dev" in out, out

    with subtest("at its time it lands, as the one who approved it"):
        machine.wait_until_succeeds(f"git -C {repo} show HEAD:settings.nix | grep -q 'a = 1'", timeout=180)
        machine.wait_until_succeeds(f"torq-broker --json log --brief | grep -q '\"scheduled\":{s['seq']}'", timeout=60)
        a = [e for e in ledger() if e["kind"] == "applied" and e["data"].get("scheduled") == s["seq"]]
        assert len(a) == 1 and a[0]["approved_by"] == "dev" and a[0]["data"]["proposal"] == p, a
        assert scheduled() == [], scheduled()
        machine.succeed("torq-broker status")

    with subtest("a commit at cancelled never lands"):
        p = propose("{ a = 1; b = 0; c = 0; }", "{ a = 1; b = 0; c = 1; }", "c, cancelled")
        s = schedule(p, 45)
        timer = s["data"]["timer"]
        # Someone else's is an administrator's to cancel; the agent's never. Its approver's own: theirs.
        code, v = ask("sam", {"args": ["--json", "cancel-scheduled", str(s["seq"])]})
        assert code == 1 and "not an administrator" in v["error"], (code, v)
        code, v = ask("torq-agent", {"args": ["--json", "cancel-scheduled", str(s["seq"])]})
        assert code == 1 and "the agent proposes" in v["error"], (code, v)
        code, v = ask("dev", {"args": ["--json", "cancel-scheduled", str(s["seq"])]})
        assert code == 0 and v["kind"] == "apply_cancelled" and v["data"]["scheduled"] == s["seq"], (code, v)
        machine.fail(f"systemctl is-active torq-commit-at-{timer}.timer")
        # Its run, by hand after the cancel (a timer that could not be stopped): nothing.
        out = machine.succeed(f"torq-broker run-scheduled {timer}")
        assert "nothing is scheduled" in out, out
        before = head()
        machine.sleep(60)
        assert head() == before and "c = 0" in setting(), setting()
        assert not [e for e in ledger() if e["kind"] == "applied" and e["data"].get("proposal") == p]

    with subtest("a commit at whose configuration moved on is refused, on the record"):
        p = propose("{ a = 1; b = 0; c = 0; }", "{ a = 1; b = 1; c = 0; }", "b, gone stale")
        s = schedule(p, 50)
        # Something else lands first: the scheduled change was prepared on what came before it.
        o = propose("{ a = 1; b = 0; c = 0; }", "{ a = 2; b = 0; c = 0; }", "a again, now")
        code, v = ask("dev", {"args": ["--json", "apply", str(o)], "password": password})
        assert code == 0, (code, v)
        moved = head()
        machine.wait_until_succeeds(f"torq-broker --json log --brief | grep apply_refused | grep -q '\"scheduled\":{s['seq']}'", timeout=180)
        r = [e for e in ledger() if e["kind"] == "apply_refused" and e["data"].get("scheduled") == s["seq"]]
        assert len(r) == 1 and "changed since it was scheduled" in r[0]["reason"], r
        assert head() == moved and "b = 0" in setting(), setting()
        assert scheduled() == []
        machine.succeed("torq-broker verify && torq-broker status")
  '';
}
