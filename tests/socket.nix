# NixOS VM test: the broker on a socket (ADR-040). Who is asking is the kernel's answer: each claim
# the ADR makes is checked here against the broker itself, over its socket, and through torqcli.
# Rebuilds are stubbed (`true`), as in the trust loop: this proves who may do what, not the build.
{ pkgs, self }:

let
  # The test's own passwords (crypt SHA-512, fixed salts): dev is the owner (wheel), sam is not.
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
  samHash = "$6$torqsocketsal2$p4ihhGlRahyPpWY43xyA52iNOzVtnXF1dQCVxf0eBeRehXODXyWOdptk3znfwCfmhj5ERP28izhmIRqrAL5J00";
in
pkgs.testers.runNixOSTest {
  name = "torq-socket";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    users.users.sam = { isNormalUser = true; hashedPasswordFile = lib.mkForce null; hashedPassword = samHash; };
    # The agent's account comes from modules/agent.nix (ADR-041); it may propose any person's
    # packages here, so that what this test checks is the socket's rules, not the agent's list.
    torq.agent.mayPropose.users = [ "dev" "sam" "torq-agent" ];
    # A rebuild that says a lot, as a real one does: every apply's answer and exit must still come
    # through a burst of the build's output (the third review of ADR-040).
    torq.broker.rebuildCommand = "seq 1 6000 >&2";
    # socat speaks to the socket; jq reads the answers; tmux is a terminal to type a password at.
    environment.systemPackages = [ pkgs.socat pkgs.jq pkgs.tmux pkgs.pam pkgs.python3Minimal ];
    # su asks at a terminal: the password typed into one, as dev (the su check, below).
    environment.etc."torq-test/su-as-dev".text = ''
      (sleep 1; printf '%s\n' "$1") | script -qec "su dev -c 'touch /tmp/su-$2'" /dev/null
    '';
    environment.etc."torq-test/desktop.nix".text = "{ nightLight = false; }\n";
    environment.etc."torq-test/system.diff".text = ''
      --- a/desktop.nix
      +++ b/desktop.nix
      @@ -1 +1 @@
      -{ nightLight = false; }
      +{ nightLight = true; }
    '';
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("torq-broker.socket")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && cp /etc/torq-test/desktop.nix {repo}/ && touch {repo}/flake.nix")
    machine.succeed("torq-broker init")

    def ask(user, request):
        """One request over the socket as `user`: (exit code, stdout, stderr), from the frames."""
        line = json.dumps(request)
        cmd = f"printf '%s\\n' {shlex.quote(line)} | socat -t 30 - UNIX-CONNECT:/run/torq/broker.sock"
        raw = machine.succeed(f"su -s /bin/sh {user} -c {shlex.quote(cmd)}")
        code, out, err = None, "", ""
        for l in raw.splitlines():
            f = json.loads(l)
            out += f.get("out", "")
            err += f.get("err", "")
            if "exit" in f:
                code = f["exit"]
        return code, out, err

    def answer(code_out_err):
        code, out, _ = code_out_err
        return code, json.loads(out) if out.strip() else None

    def own_diff(name):
        return f"--- /dev/null\n+++ b/users/{name}/packages.json\n@@ -0,0 +1 @@\n+[\"hello\"]\n"

    def head():
        return machine.succeed(f"git -C {repo} rev-parse HEAD").strip()

    def propose(user, diff, reason):
        code, v = answer(ask(user, {"args": ["--json", "propose", f"--base={head()}", f"--reason={reason}"], "diff": diff}))
        assert code == 0, (code, v)
        return v

    with subtest("the socket is anyone's to reach; what each may do is decided inside"):
        machine.succeed("test \"$(stat -c %a /run/torq/broker.sock)\" = 666")
        # Reading is open to every account, the agent's included (no sudo, nothing secret).
        for who in ["dev", "sam", "torq-agent"]:
            code, v = answer(ask(who, {"args": ["--json", "verify"]}))
            assert code == 0 and v["entries"] == 1, (who, code, v)
        # Root runs the broker itself: not through the socket.
        code, v = answer(ask("root", {"args": ["--json", "verify"]}))
        assert code == 1 and "root runs torq-broker itself" in v["error"], v

    with subtest("no caller names itself, a file, or the broker's settings"):
        for args in [["--json", "apply", "2", "--approved-by=dev"], ["--json", "propose", "--actor", "user"],
                     ["--json", "propose", "--diff=/etc/shadow"], ["--json", "password", "--user", "sam", "--hash-file", "/etc/shadow"],
                     ["--json", "--rebuild-cmd=touch /tmp/pwned", "status"], ["--json", "--state-dir=/tmp/x", "log"]]:
            code, v = answer(ask("dev", {"args": args}))
            assert code == 1 and "not taken from a caller" in v["error"], (args, code, v)
        machine.succeed("test ! -e /tmp/pwned")
        # Fields the broker does not read are not quietly dropped: the request is refused.
        code, out, err = ask("dev", {"args": ["verify"], "actor": "dev"})
        assert code == 2 and "not one the broker reads" in err, (code, out, err)

    with subtest("a person's own settings are theirs: no password, no sudo"):
        p = propose("sam", own_diff("sam"), "hello for sam")
        assert p["actor"] == "user" and p["tier"] == "user" and p["data"]["via"] == "socket" and p["data"]["caller"] == "sam", p
        r = ask("sam", {"args": ["--json", "apply", str(p["seq"])]})
        code, v = answer(r)
        # The build's 6000 lines: a reader keeping up gets them, or is told how many were left out —
        # and always the answer and the exit code.
        assert "\n6000\n" in r[2] or "not shown" in r[2], r[2][-300:]
        assert code == 0 and v["approved_by"] == "sam" and v["data"]["via"] == "socket", (code, v)
        # A commit confirmed holds everyone's changes back while it waits: an administrator's,
        # even for their own folder (the audit's 2).
        code, v = answer(ask("sam", {"args": ["--json", "apply", str(p["seq"]), "--confirm", "1440"]}))
        assert code == 1 and "not an administrator" in v["error"], (code, v)
        # Someone else's are not theirs (KNOWN-GAPS 6a): sam cannot apply dev's.
        p = propose("sam", own_diff("dev"), "hello for dev, by sam")
        code, v = answer(ask("sam", {"args": ["--json", "apply", str(p["seq"])]}))
        assert code == 1 and "not an administrator" in v["error"], (code, v)

    with subtest("a person's apps are their own setting: no password, on the record, and only real ids"):
        def apps(name, body):
            return f"--- /dev/null\n+++ b/users/{name}/flatpaks.json\n@@ -0,0 +1 @@\n+{body}\n"
        p = propose("sam", apps("sam", '["org.mozilla.firefox"]'), "an app")
        code, v = answer(ask("sam", {"args": ["--json", "apply", str(p["seq"])]}))
        assert code == 0, (code, v)
        machine.succeed(f"git -C {repo} show HEAD:users/sam/flatpaks.json | grep -q org.mozilla.firefox")
        code, v = answer(ask("dev", {"args": ["--json", "propose", f"--base={head()}", "--reason=not an id"], "diff": apps("dev", '["firefox"]')}))
        assert code == 1, (code, v)

    with subtest("a person's desktop is their own setting: COSMIC's names only, never a path"):
        def desk(name, body):
            return f"--- /dev/null\n+++ b/users/{name}/desktop.json\n@@ -0,0 +1 @@\n+{body}\n"
        good = '{"save": "automatic", "cosmic": {"com.system76.CosmicPanel.Panel/v1/size": "M"}}'
        p = propose("sam", desk("sam", good), "a desktop")
        code, v = answer(ask("sam", {"args": ["--json", "apply", str(p["seq"])]}))
        assert code == 0, (code, v)
        machine.succeed(f"git -C {repo} show HEAD:users/sam/desktop.json | grep -q CosmicPanel")
        bad = '{"cosmic": {"../../.bashrc/v1/x": "evil"}}'
        code, v = answer(ask("dev", {"args": ["--json", "propose", f"--base={head()}", "--reason=a path"], "diff": desk("dev", bad)}))
        assert code == 1, (code, v)

    with subtest("a secret's value (ADR-052): encrypted, root's alone, its owner's, off the record, never the agent's"):
        store = "/var/lib/torq/secrets/store"
        def secret(user, cmd, name, owner=None, **extra):
            args = ["--json", f"secret-{cmd}", "--name", name] + (["--user", owner] if owner else [])
            return answer(ask(user, {"args": args, **extra}))
        # A person's own: set with no password, by them alone.
        code, v = secret("sam", "set", "gh", "sam", key="sam-token-1")
        assert code == 0 and v["kind"] == "secret_set" and v["tier"] == "user" and v["approved_by"] == "sam", (code, v)
        f = f"{store}/users/sam/gh"
        mode = machine.succeed(f"stat -c %a:%U {f}").strip()
        assert mode == "600:root", mode
        mode = machine.succeed(f"stat -c %a:%U {store}/users/sam").strip()
        assert mode == "700:root", mode
        machine.fail(f"grep -q sam-token-1 {f}")
        # Encrypted by systemd-creds itself, bound to its name: it decrypts as that and nothing else.
        assert machine.succeed(f"systemd-creds decrypt --name=torq-secret.user.sam.gh {f} -").strip() == "sam-token-1"
        machine.fail(f"systemd-creds decrypt --name=torq-secret.system.gh {f} -")
        # Read back: with their password, and only theirs.
        code, v = secret("sam", "show", "gh", "sam")
        assert code == 77, (code, v)
        code, v = secret("sam", "show", "gh", "sam", password="Sam-sock-7742")
        assert code == 0 and v["value"] == "sam-token-1", (code, v)
        for who, pw in [("dev", "Torq-sock-2210"), ("torq-agent", None)]:
            for cmd in ["set", "show"]:
                code, v = secret(who, cmd, "gh", "sam", key="other", password=pw)
                assert code == 1, (who, cmd, code, v)
        assert machine.succeed(f"systemd-creds decrypt --name=torq-secret.user.sam.gh {f} -").strip() == "sam-token-1", "unchanged"
        # The system's: an administrator's, with their password; not a person's, nor the agent's.
        code, v = secret("dev", "set", "ci", key="ci-token-2")
        assert code == 77, (code, v)
        code, v = secret("dev", "set", "ci", key="ci-token-2", password="Torq-sock-2210")
        assert code == 0 and v["tier"] == "system", (code, v)
        for who in ["sam", "torq-agent"]:
            for cmd in ["set", "show", "delete"]:
                code, v = secret(who, cmd, "ci", key="x")
                assert code == 1, (who, cmd, code, v)
        # The agent sees which secrets there are, never a value, nor the store.
        code, v = answer(ask("torq-agent", {"args": ["--json", "secrets"]}))
        assert code == 0 and {"owner": "system", "name": "ci"} in v and {"owner": {"person": "sam"}, "name": "gh"} in v, (code, v)
        machine.fail(f"su -s /bin/sh torq-agent -c 'ls {store}'")
        machine.fail(f"su -s /bin/sh torq-agent -c 'cat {f}'")
        # Deleted: their own by them; anyone's by an administrator, with their password.
        code, v = secret("sam", "set", "old", "sam", key="old-3")
        assert code == 0, (code, v)
        code, v = secret("dev", "delete", "old", "sam", password="Torq-sock-2210")
        assert code == 0 and v["kind"] == "secret_deleted", (code, v)
        code, v = secret("sam", "delete", "gh", "sam")
        assert code == 0, (code, v)
        machine.fail(f"test -e {f}")
        # On the record, every time, without a value.
        record = machine.succeed("torq-broker log")
        for value in ["sam-token-1", "ci-token-2", "old-3"]:
            assert value not in record, value
        for kind in ["secret_set", "secret_shown", "secret_deleted"]:
            assert kind in record, kind

    with subtest("a secret's rules (ADR-052): the person's own, checked, never the agent's or an administrator's"):
        def rules(name, body):
            return f"--- /dev/null\n+++ b/users/{name}/secrets.json\n@@ -0,0 +1 @@\n+{body}\n"
        good = '{"gh": {"kind": "token", "https": {"hosts": ["api.github.com"]}, "actors": ["torq-agent"]}}'
        p = propose("sam", rules("sam", good), "the agent may use gh")
        code, v = answer(ask("sam", {"args": ["--json", "apply", str(p["seq"])]}))
        assert code == 0 and v["approved_by"] == "sam", (code, v)
        def changed(path, new):
            return f"--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-{good}\n+{new}\n"
        # An administrator's password does not change anyone else's.
        # Naming another person in a person's own rules is no rules file of theirs: refused when proposed.
        code, v = answer(ask("sam", {"args": ["--json", "propose", f"--base={head()}", "--reason=dev too"], "diff": changed("users/sam/secrets.json", good.replace('"torq-agent"', '"torq-agent", "dev"'))}))
        assert code == 1, (code, v)
        p = propose("dev", changed("users/sam/secrets.json", good.replace('"api.github.com"', '"api.github.com", "git.example.org"')), "dev's say over sam's")
        code, v = answer(ask("dev", {"args": ["--json", "apply", str(p["seq"])], "password": "Torq-sock-2210"}))
        assert code == 1 and "theirs alone" in v["error"], (code, v)
        # Nothing but rules: an address is no host; a field nothing reads is refused.
        for body in ['{"t": {"kind": "token", "https": {"hosts": ["10.0.0.1"]}}}', '{"t": {"kind": "token", "run": "sh"}}']:
            code, v = answer(ask("dev", {"args": ["--json", "propose", f"--base={head()}", "--reason=bad"], "diff": rules("dev", body)}))
            assert code == 1, (body, code, v)
        sysrules = "--- /dev/null\n+++ b/secrets/system.json\n@@ -0,0 +1 @@\n+{\"ci\": {\"kind\": \"token\", \"https\": {\"hosts\": [\"localhost\"]}}}\n"
        code, v = answer(ask("dev", {"args": ["--json", "propose", f"--base={head()}", "--reason=bad"], "diff": sysrules}))
        assert code == 1 and "secrets/system.json" in v["error"], (code, v)
        # The agent proposes neither file, even for a person whose packages it may propose.
        for diff in [changed("users/sam/secrets.json", good.replace('"api.github.com"', '"api.github.com", "evil.example.com"')),
                     sysrules.replace("localhost", "api.github.com")]:
            code, v = answer(ask("torq-agent", {"args": ["--json", "propose", f"--base={head()}", "--reason=wider"], "diff": diff}))
            assert code == 1, (code, v)

    with subtest("a refused or busy request is an answer, not a failed unit: a switch must not fail for one"):
        machine.succeed("test -z \"$(systemctl list-units --failed --no-legend 'torq-broker@*')\"")

    with subtest("a caller that handed its connection on and left cannot use the password path"):
        # Who connected has gone: there is no record of where it was, so it must not pass for a terminal.
        p = propose("dev", machine.succeed("cat /etc/torq-test/system.diff"), "handed on")
        request = json.dumps({"args": ["--json", "apply", str(p["seq"])], "password": "Torq-sock-2210"})
        script = (
            "import os,socket,sys,time\n"
            "s=socket.socket(socket.AF_UNIX);s.connect('/run/torq/broker.sock')\n"
            "if os.fork():os._exit(0)\n"
            "time.sleep(1)\n"
            "s.sendall(sys.argv[1].encode()+b'\\n')\n"
            "d=bytes()\n"
            "while True:\n"
            " c=s.recv(65536)\n"
            " if not c:break\n"
            " d+=c\n"
            "print(d.decode())\n"
        )
        machine.succeed(f"printf %s {shlex.quote(script)} > /tmp/handed-on.py && chmod 644 /tmp/handed-on.py")
        out = machine.succeed(f"su -s /bin/sh dev -c {shlex.quote('python3 /tmp/handed-on.py ' + shlex.quote(request) + ' > /tmp/handed-on.out; true')}; sleep 3; cat /tmp/handed-on.out")
        assert "the caller has gone" in out, out

    with subtest("a system change asks an administrator's password, checked by PAM"):
        p = propose("dev", machine.succeed("cat /etc/torq-test/system.diff"), "night light")
        seq = str(p["seq"])
        assert p["tier"] == "system", p
        # Not an administrator: refused, whatever the password.
        code, v = answer(ask("sam", {"args": ["--json", "apply", seq], "password": "Sam-sock-7742"}))
        assert code == 1 and "not an administrator" in v["error"], (code, v)
        # The owner: without the password, asked for it (77); a wrong one, refused and counted.
        code, v = answer(ask("dev", {"args": ["--json", "apply", seq]}))
        assert code == 77 and v["password"] and "needs dev's password" in v["error"], (code, v)
        code, v = answer(ask("dev", {"args": ["--json", "apply", seq], "password": "wrong-guess"}))
        assert code == 77 and "not dev's password" in v["error"], (code, v)
        machine.succeed("test $(faillock --dir /run/torq/faillock --user dev | grep -c '^[0-9]') -ge 1")
        machine.succeed(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = false'")
        # The right one: applied, ledgered as dev's, through the socket.
        code, v = answer(ask("dev", {"args": ["--json", "apply", seq], "password": "Torq-sock-2210"}))
        assert code == 0 and v["approved_by"] == "dev" and v["data"]["via"] == "socket", (code, v)
        night = v["seq"]
        machine.succeed(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")
        machine.succeed("faillock --dir /run/torq/faillock --user dev --reset")

    with subtest("guessing stops: five wrong, locked, even with the right one"):
        # Someone else's folder: an administrator's password.
        p = propose("dev", own_diff("torq-agent"), "for the agent's folder, by dev")
        for _ in range(5):
            code, _v = answer(ask("dev", {"args": ["--json", "apply", str(p["seq"])], "password": "guess"}))
            assert code == 77
        code, v = answer(ask("dev", {"args": ["--json", "apply", str(p["seq"])], "password": "Torq-sock-2210"}))
        assert code == 77, ("locked out", code, v)
        # The count is root's: the person cannot reset it, by the tool or by hand (the audit's 1).
        machine.fail("su - dev -c 'faillock --dir /run/torq/faillock --user dev --reset'")
        machine.fail("su - dev -c ': > /run/torq/faillock/dev'")
        machine.fail("su - dev -c 'ls /run/torq/faillock'")
        code, v = answer(ask("dev", {"args": ["--json", "apply", str(p["seq"])], "password": "Torq-sock-2210"}))
        assert code == 77, ("still locked", code, v)
        machine.succeed("faillock --dir /run/torq/faillock --user dev --reset")
        code, v = answer(ask("dev", {"args": ["--json", "apply", str(p["seq"])], "password": "Torq-sock-2210"}))
        assert code == 0, (code, v)

    with subtest("a proposal says what it was prepared against: nothing else is judged"):
        # Without it a diff can be nothing to another's file at one commit and something at the next.
        code, v = answer(ask("sam", {"args": ["--json", "propose", "--reason=x"], "diff": own_diff("sam")}))
        assert code != 0 and "--base" in v["error"], (code, v)

    with subtest("a service's account proposes nothing; a caller that sends nothing is let go"):
        code, v = answer(ask("nobody", {"args": ["--json", "propose", f"--base={head()}", "--reason=x"], "diff": own_diff("nobody")}))
        assert code == 1 and "not a person's account" in v["error"], (code, v)
        code, v = answer(ask("nobody", {"args": ["--json", "verify"]}))
        assert code == 0, (code, v)
        # Silent for longer than the limit: the broker hangs up on it (the audit's 4).
        out = machine.succeed("su -s /bin/sh sam -c 'sleep 15 | timeout 40 socat -t 40 - UNIX-CONNECT:/run/torq/broker.sock'")
        assert '"exit":2' in out and 'reading the request' in out, out
        # Four connections per account: a fifth waits for none of them, it is refused at once.
        machine.succeed("for i in 1 2 3 4; do su -s /bin/sh sam -c 'sleep 8 | socat -t 20 - UNIX-CONNECT:/run/torq/broker.sock' >/dev/null 2>&1 & done; sleep 2")
        fifth = f"printf '%s\\n' {shlex.quote(json.dumps({'args': ['verify']}))} | socat -t 5 - UNIX-CONNECT:/run/torq/broker.sock"
        out = machine.execute(f"su -s /bin/sh sam -c {shlex.quote(fifth)} 2>&1")[1]
        assert '"exit"' not in out, out
        # Another account is not held up by sam's.
        code, v = answer(ask("dev", {"args": ["--json", "verify"]}))
        assert code == 0, (code, v)
        machine.sleep(10)

    with subtest("the agent proposes, and can do nothing else"):
        p = propose("torq-agent", own_diff("dev"), "the agent suggests hello")
        assert p["actor"] == "agent" and p["data"]["caller"] == "torq-agent", p
        for args in [["apply", str(p["seq"])], ["revert", "3"], ["confirm"], ["reconcile"], ["repair"], ["password", "--user", "dev"]]:
            code, v = answer(ask("torq-agent", {"args": ["--json"] + args, "password": "Torq-sock-2210", "hash": "$6$x$y"}))
            assert code == 1 and "the agent proposes; a person applies" in v["error"], (args, code, v)
        # The machine's own commands are not on the socket for anyone.
        for args in [["init"], ["setup"], ["expire-confirm"], ["serve"]]:
            code, v = answer(ask("dev", {"args": ["--json"] + args}))
            assert code == 1 and "not run over the socket" in v["error"], (args, code, v)

    with subtest("torqcli: no sudo to read the ledger; a system revert asks the password at the terminal"):
        machine.fail("su - sam -c 'sudo -n true'")
        out = machine.succeed("su - sam -c 'torqcli show system ledger'")
        assert "hello for sam" in out and "night light" in out, out
        # Without a terminal: nobody to ask, nothing changed.
        before = head()
        machine.fail(f"su - dev -c 'torqcli request system revert {night}' < /dev/null")
        assert head() == before
        # At a terminal: the question, then the password — typed, not echoed.
        machine.succeed(f"su - dev -c \"tmux new-session -d -s c -x 160 -y 40 'torqcli request system revert {night}; sleep 600'\"")
        pane = lambda: machine.succeed("su - dev -c 'tmux capture-pane -p -t c'")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t c' | grep -q 'y/N'", timeout=120)
        machine.succeed("su - dev -c 'tmux send-keys -t c y Enter'")
        machine.wait_until_succeeds("su - dev -c 'tmux capture-pane -p -t c' | grep -q 'Password for dev:'", timeout=120)
        machine.succeed("su - dev -c 'tmux send-keys -t c Torq-sock-2210 Enter'")
        machine.wait_until_succeeds(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = false'", timeout=120)
        # The commit comes first, the ledger's entry once the rebuild is done: wait for that.
        machine.wait_until_succeeds("torq-broker log --json --brief | tail -1 | jq -e '.kind == \"reverted\"'", timeout=120)
        last = json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))
        assert last["kind"] == "reverted" and last["approved_by"] == "dev" and last["data"]["via"] == "socket", last
        assert "Torq-sock-2210" not in pane(), pane()

    with subtest("a caller that hangs up stops nothing"):
        p = propose("dev", own_diff("dev").replace("hello", "jq"), "jq for dev")
        line = json.dumps({"args": ["--json", "apply", str(p["seq"])]})
        # Send, and leave at once: the change still lands, and is ledgered.
        send = f"printf '%s\\n' {shlex.quote(line)} | timeout 0.3 socat - UNIX-CONNECT:/run/torq/broker.sock || true"
        machine.succeed(f"su -s /bin/sh dev -c {shlex.quote(send)}")
        machine.wait_until_succeeds(f"torq-broker log --json --brief | jq -e 'select(.kind == \"applied\" and .data.proposal == {p['seq']})'", timeout=60)
        machine.succeed("torq-broker verify && torq-broker status")
        # No request's files are left behind.
        machine.succeed("test -z \"$(ls /var/lib/torq/state | grep '^request-' || true)\"")

    with subtest("sudo stays the recovery door, and is ledgered as what root says about itself"):
        # sudo sets SUDO_UID; root at a shell, and the machine's own units, do not.
        machine.succeed(f"printf %s {shlex.quote(own_diff('nobody'))} > /tmp/by-hand.diff")
        v = json.loads(machine.succeed(f"SUDO_UID=1000 torq-broker --json propose --actor user --base {head()} --diff /tmp/by-hand.diff --reason by-hand"))
        assert v["data"]["via"] == "sudo", v
        v = json.loads(machine.succeed(f"torq-broker --json propose --actor user --base {head()} --diff /tmp/by-hand.diff --reason by-root"))
        assert "via" not in v["data"], v
        machine.succeed("torq-broker verify")

    with subtest("sudo, su and passwd lock as the broker does: five wrong, ten minutes, counted by root"):
        machine.succeed("faillock --dir /run/torq/faillock --user dev --reset")
        sudo = lambda pw: machine.execute(f"su - dev -c {shlex.quote(f'printf %s {shlex.quote(pw)} | sudo -S -k -p \"\" true')}")[0]
        assert sudo("Torq-sock-2210") == 0, "the right password works"
        for _ in range(5):
            assert sudo("guess") != 0
        assert sudo("Torq-sock-2210") != 0, "locked: even the right one"
        # The same count as the broker's: locked there too.
        code, v = answer(ask("dev", {"args": ["--json", "apply", "1"], "password": "Torq-sock-2210"}))
        assert code in (1, 77), (code, v)
        machine.fail("su - dev -c 'faillock --dir /run/torq/faillock --user dev --reset'")
        machine.succeed("faillock --dir /run/torq/faillock --user dev --reset")
        assert sudo("Torq-sock-2210") == 0, "unlocked by root"
        # su is root's alone: another account cannot spend dev's guesses, nor guess them (the audit of
        # M3, M3). Refused before any password is asked: the count does not move.
        def su(pw, mark):
            machine.execute(f"rm -f /tmp/su-{mark}")
            machine.execute(f"su - sam -c {shlex.quote(f'sh /etc/torq-test/su-as-dev {shlex.quote(pw)} {mark}')}")
            return machine.execute(f"test -e /tmp/su-{mark}")[0] == 0
        for i in range(7):
            assert not su("guess", f"wrong{i}")
        assert not su("Torq-sock-2210", "right"), "not even with the right password"
        assert machine.succeed("faillock --dir /run/torq/faillock --user dev | grep -c '^[0-9]' || true").strip() == "0"
        assert sudo("Torq-sock-2210") == 0, "dev's own doors still open"
        machine.succeed("su - dev -c true")

        for svc in ["sudo", "passwd"]:
            stack = machine.succeed(f"grep -E '^auth' /etc/pam.d/{svc}")
            assert "pam_faillock.so preauth dir=/run/torq/faillock deny=5 unlock_time=600" in stack and "authfail dir=/run/torq/faillock" in stack, (svc, stack)
            assert "pam_faillock.so required" not in stack.replace("  ", " "), stack
  '';
}
