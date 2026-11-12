# NixOS VM test: `torq agent run` (ADR-041, 5) — a program run as the agent's account, as a person
# would run Claude Code: who it is, what it sees, the one directory shared with it, what it can
# change (it proposes), and that a sandbox of its own still works inside the launcher's.
{ pkgs, self }:

let
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
in
pkgs.testers.runNixOSTest {
  name = "torq-agent-run";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    users.users.sam = { isNormalUser = true; hashedPasswordFile = lib.mkForce null; hashedPassword = "!"; };
    torq.broker.rebuildCommand = "true";
    environment.systemPackages = [ pkgs.socat pkgs.jq pkgs.bubblewrap ];
    # Room for the many files the sweep race below walks (the default disk runs out of inodes at ~75k).
    virtualisation.diskSize = 4096;
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    machine.wait_for_unit("torq-broker.socket")
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")
    machine.succeed("su - dev -c 'mkdir -p ~/src/project ~/other && echo secret > ~/secret && echo mine > ~/src/project/README'")

    def as_dev(cmd, ok=True):
        code, out = machine.execute(f"su - dev -c {shlex.quote(cmd)} 2>&1")
        assert (code == 0) == ok, f"{cmd}: exit {code}, expected {'success' if ok else 'failure'}:\n{out}"
        return out

    def agent(inner, share=None, ok=True):
        s = f"--share {share} " if share else ""
        return as_dev(f"torq agent run {s}-- sh -c {shlex.quote(inner)}", ok)

    with subtest("it runs as the agent, in the agent's home, without a password"):
        out = agent("id -un; echo $HOME; pwd; touch ~/state && stat -c %U ~/state")
        assert out.split() == ["torq-agent", "/var/lib/torq-agent", "/var/lib/torq-agent", "torq-agent"], out
        # No sudo from there, and no password was asked (sudo -n would have failed otherwise).
        agent("sudo -n true", ok=False)

    with subtest("it sees no home, no one's runtime directory, a /tmp of its own"):
        machine.succeed("su - dev -c 'echo t > /tmp/dev-tmp'")
        out = agent("ls -A /home; ls -A /run/user; ls -A /tmp; cat /home/dev/secret 2>/dev/null || echo HIDDEN")
        assert out.split() == ["HIDDEN"], out

    with subtest("one directory shared: read and write it, nothing beside it"):
        out = agent("pwd; cat README; echo agent > made-by-agent; ls -A /home/dev; cat /home/dev/secret 2>/dev/null || echo HIDDEN", share="/home/dev/src/project")
        lines = out.splitlines()
        assert all(x in lines for x in ["/home/dev/src/project", "mine", "src", "HIDDEN"]) and "secret" not in lines, out
        assert "Shared with the agent: /home/dev/src/project" in out, out
        # What the agent made stays the person's to change, and theirs the agent's (default ACLs).
        as_dev("echo dev >> ~/src/project/made-by-agent && cat ~/src/project/made-by-agent | grep -q dev")
        agent("echo again >> made-by-agent && echo again >> README", share="/home/dev/src/project")
        # Outside the launcher, the agent has no way into the home at all — not even to the shared
        # directory by its name: it was handed the directory, open, not a path (and the home has no
        # ACL of the agent's).
        machine.fail("su -s /bin/sh torq-agent -c 'test -d /home/dev/src/project'")
        machine.fail("getfacl -p /home/dev /home/dev/src 2>/dev/null | grep -q torq-agent")
        machine.fail("su -s /bin/sh torq-agent -c 'ls /home/dev'")

        machine.fail("su -s /bin/sh torq-agent -c 'cat /home/dev/secret'")
        machine.fail("su -s /bin/sh torq-agent -c 'ls /home/dev/other'")

    with subtest("handed the directory, not a way to it: no descriptor of it left, no privilege to gain (the audit of M3, M6)"):
        # Every descriptor the agent's program holds, and what its process may do.
        out = agent("for f in /proc/$$/fd/*; do readlink $f; done; grep -E '^(NoNewPrivs|CapEff|CapPrm|CapBnd):' /proc/$$/status", share="/home/dev/src/project")
        fds = [l for l in out.splitlines() if l.startswith("/")]
        assert not any(d.startswith(("/home", "/run/torq-agent-stage")) for d in fds), out
        status = dict(l.split(":\t") for l in out.splitlines() if ":\t" in l)
        assert status.get("NoNewPrivs") == "1", out
        assert status.get("CapEff") == "0000000000000000" and status.get("CapPrm") == "0000000000000000", out

    with subtest("only one's own directory, inside one's home, and not the home itself"):
        for share in ["/home/dev", "/etc", "/home/dev/secret", "/var/lib/torq"]:
            out = as_dev(f"torq agent run --share {share} -- true", ok=False)
            assert "share" in out or "not" in out, (share, out)
        machine.succeed("mkdir /home/dev/rootdir")
        out = as_dev("torq agent run --share /home/dev/rootdir -- true", ok=False)
        assert "not yours to share" in out, out
        # Only an administrator starts it; the launcher refuses anyone but the agent.
        machine.fail("su - sam -c 'torq agent run -- true'")
        out = machine.fail("su - dev -c '/run/current-system/sw/bin/torq-agent-run -- true' 2>&1")
        assert "is started by torq agent run" in out, out
        # The launcher's root step checks what it is handed: the open directory must be the one named,
        # and the person's own — or nothing runs.
        launcher = machine.succeed("readlink -f /run/current-system/sw/bin/torq-agent-run").strip()
        for fd_of, named in [("/home/dev/src/project", "/home/dev/other"), ("/home/dev/rootdir", "/home/dev/rootdir")]:
            out = as_dev(f"sudo -C 4 {launcher} --share-fd 3 {named} -- touch {named}/ran 3< {fd_of}", ok=False)
            assert "is not" in out, (fd_of, named, out)
        machine.fail("test -e /home/dev/other/ran -o -e /home/dev/rootdir/ran")

    with subtest("what it changes, it proposes; a person applies"):
        req = json.dumps({"args": ["--json", "propose", "--base=" + machine.succeed(f"git -C {repo} rev-parse HEAD").strip(), "--reason=hello for dev"],
                          "diff": "--- /dev/null\n+++ b/users/dev/packages.json\n@@ -0,0 +1 @@\n+[\"hello\"]\n"})
        out = agent(f"printf '%s\\n' {shlex.quote(req)} | socat -t 30 - UNIX-CONNECT:/run/torq/broker.sock")
        e = json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))
        assert e["kind"] == "proposal" and e["actor"] == "agent" and e["data"]["caller"] == "torq-agent", (e, out)


    with subtest("nothing it starts outlives it; one session at a time; nothing setuid; the homes stay hidden"):
        # A process it leaves behind, detached, ends with the session (its own process namespace).
        agent("setsid sleep 300 > /dev/null 2>&1 < /dev/null & echo left")
        machine.wait_until_fails("pgrep -u torq-agent", timeout=20)
        # While a session runs, no other may share a directory (its programs could race the ACLs).
        machine.succeed("su - dev -c 'torq agent run -- sleep 30' > /dev/null 2>&1 &")
        machine.wait_until_succeeds("pgrep -u torq-agent sleep", timeout=20)
        out = as_dev("torq agent run --share /home/dev/src/project -- true", ok=False)
        assert "running already" in out, out
        machine.succeed("pkill -u torq-agent sleep || true")
        machine.wait_until_fails("pgrep -u torq-agent", timeout=20)
        # The share is nosuid and nodev; the hidden homes cannot be uncovered from the agent's own
        # namespaces (the mounts it was given are locked under it).
        out = agent("findmnt -no OPTIONS --target .", share="/home/dev/src/project")
        assert "nosuid" in out and "nodev" in out, out
        agent("unshare -Urm umount /home", ok=False)
        agent("unshare -Urm umount /home/dev/src/project", ok=False)
        # What it leaves set-id is made plain when it ends: the person's view of the folder is not nosuid
        # (the audit of M3, L2).
        out = agent("cp /run/current-system/sw/bin/true ./sid && chmod 4755 ./sid && stat -c %a ./sid", share="/home/dev/src/project")
        assert "set-id bits taken off" in out and "sid" in out, out
        assert machine.succeed("stat -c %a /home/dev/src/project/sid").strip() == "755"

    with subtest("the sweep cannot be led out of the folder by a link (KNOWN-GAPS 6zz)"):
        # The agent leaves a set-id file named sudo in a folder with many others (a long walk for find); the
        # moment the sweep starts, the person swaps that folder for a link to sudo's wrapper's own. find has
        # the path already and chmod goes by it again: as root, it would take the bits off sudo's wrapper.
        wrapper = "/run/wrappers/bin/sudo"
        mode = machine.succeed(f"stat -c %a {wrapper}").strip()
        assert mode.startswith(("4", "6")), mode
        machine.succeed("su - dev -c 'cd ~/src/project && (until pgrep -x find > /dev/null; do :; done; mv d d.real && ln -s /run/wrappers/bin d) > /dev/null 2>&1 &'")
        agent("mkdir d && cd d && cp /run/current-system/sw/bin/true sudo && chmod 4755 sudo && seq 200000 | xargs touch", share="/home/dev/src/project")
        machine.succeed("test -L /home/dev/src/project/d")  # the swap happened while the sweep ran
        assert machine.succeed(f"stat -c %a {wrapper}").strip() == mode, "the sweep changed sudo's wrapper"
        machine.succeed("rm -rf /home/dev/src/project/d /home/dev/src/project/d.real")
    with subtest("a sandbox of its own still runs inside (as Claude Code's does)"):
        out = agent("bwrap --ro-bind / / --dev /dev --proc /proc --unshare-all --die-with-parent sh -c 'id -un; echo nested'")
        assert "nested" in out, out
  '';
}
