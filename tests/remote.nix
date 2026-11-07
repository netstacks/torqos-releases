# NixOS VM test: the config repo's remote (ADR-036) — pushed after every change, the ledger anchored
# there as the branch `torq-ledger`. A ledger cut short here, one rewritten and pushed on top by root,
# one set beside the remote's history in a merge — each an alarm, and a sticky one; an unreachable or
# stalled remote never fails a change, nor holds the broker. A second machine is the remote: a bare
# repository behind SSH that, as branch protection would, refuses to rewrite or delete.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-remote";

  nodes.gitserver = { pkgs, ... }: {
    services.openssh.enable = true;
    # bash, so that a forced command can stand in for a remote that stalls (below).
    users.users.git = { isNormalUser = true; shell = pkgs.bash; };
    environment.systemPackages = [ pkgs.git ];
  };

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = "!";
    users.allowNoPasswordLogin = true; # (a test: nobody logs in by hand)
    torq.broker.rebuildCommand = "true";
    # A commit confirmed with no real build: the system it "builds" is the one running.
    torq.broker.buildCommand = "readlink -f /run/current-system";
    torq.broker.activateCommand = "true";
    torq.broker.bootCommand = "true";
    torq.broker.scheduleCommand = "true";
    torq.remote.url = "ssh://git@gitserver/home/git/config.git";
    environment.systemPackages = [ pkgs.jq pkgs.git ];
  };

  testScript = ''
    import json, shlex, time
    start_all()
    gitserver.wait_for_unit("sshd.service")
    fresh_remote = "git init -q --bare config.git && git -C config.git config receive.denyNonFastForwards true && git -C config.git config receive.denyDeletes true"
    gitserver.succeed(f"su - git -c {shlex.quote(fresh_remote + ' && mkdir -p ~/.ssh && chmod 700 ~/.ssh')}")
    machine.wait_for_unit("multi-user.target")
    repo = "/var/lib/torq/config"
    remote = "/home/git/config.git"
    url = "ssh://git@gitserver/home/git/config.git"
    ssh = "ssh -F /dev/null -i /var/lib/torq/state/remote/key -o UserKnownHostsFile=/var/lib/torq/state/remote/known_hosts -o BatchMode=yes"
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")

    def change(n):
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        diff = f"--- a/desktop.json\n+++ b/desktop.json\n@@ -1 +1 @@\n-{machine.succeed(f'cat {repo}/desktop.json').strip()}\n+{{\"n\": {n}}}\n"
        machine.succeed(f"printf %s {shlex.quote(diff)} > /tmp/c.diff")
        p = json.loads(machine.succeed(f"torq-broker --json propose --actor user --base {head} --diff /tmp/c.diff --reason 'change {n}'"))
        return json.loads(machine.succeed(f"torq-broker --json apply {p['seq']} --approved-by dev"))

    def last():
        return json.loads(machine.succeed("cat /var/lib/torq/state/remote/*/last.json"))

    def remote_git(cmd):
        return gitserver.succeed(f"git -c safe.directory='*' -C {remote} {cmd}")

    def remote_ledger():
        return remote_git("cat-file blob refs/heads/torq-ledger:ledger.jsonl")

    def local_ledger():
        return machine.succeed("cat /var/lib/torq/state/ledger.jsonl")

    def root_pushes(script):
        # Root on the machine, with the machine's key: what the broker's checks must see through.
        machine.succeed(f"cd {repo} && export GIT_SSH_COMMAND={shlex.quote(ssh)} && {script}")

    def commit_ledger(parents):
        ps = " ".join(f"-p {p}" for p in parents)
        return ("blob=$(git hash-object -w /var/lib/torq/state/ledger.jsonl) && "
                "tree=$(printf '100644 blob %s\\tledger.jsonl\\n' $blob | git mktree) && "
                f"git -c user.name=root -c user.email=root@machine commit-tree $tree {ps} -m rewritten")

    with subtest("no key yet: a change still lands, and says so"):
        change(1)
        assert not last()["ok"] and "no key yet" in last()["error"], last()
        machine.succeed("torq-broker status")

    with subtest("the key is the machine's, root's alone; added to the remote, every change is pushed"):
        key = json.loads(machine.succeed("torq-broker --json remote-key"))["public_key"]
        assert key.startswith("ssh-ed25519 ") and key == json.loads(machine.succeed("torq-broker --json remote-key"))["public_key"], "made once, kept"
        machine.fail("su - dev -c 'cat /var/lib/torq/state/remote/key'")
        machine.fail("su -s /bin/sh torq-agent -c 'cat /var/lib/torq/state/remote/key'")
        gitserver.succeed(f"echo {shlex.quote(key)} > /home/git/.ssh/authorized_keys && chown git /home/git/.ssh/authorized_keys && chmod 600 /home/git/.ssh/authorized_keys")
        for n in [2, 3]:
            change(n)
            assert last()["ok"], last()
            assert remote_git("rev-parse refs/heads/main").strip() == machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
            assert remote_ledger() == local_ledger()
        # The ledger on the remote is a branch, one line of commits; the remote's host key, pinned.
        assert remote_git("rev-list --count --min-parents=2 refs/heads/torq-ledger").strip() == "0"
        machine.succeed("grep -q gitserver /var/lib/torq/state/remote/known_hosts")
        out = machine.succeed("su - dev -c 'torqcli show system remote'")
        assert f"Remote: {url}" in out and "Last push" in out and "as they are here" in out, out

    with subtest("out of reach, or stalled: the change lands, the broker is not held, the retry catches up"):
        gitserver.succeed("systemctl stop sshd")
        change(4)
        assert not last()["ok"] and last()["anchor"]["state"] == "unreachable", last()
        machine.succeed("torq-broker status")  # late is not an alarm
        gitserver.succeed("systemctl start sshd")
        gitserver.wait_for_unit("sshd.service")
        # A remote that answers but never finishes (a forced command that sleeps): a minute at most.
        gitserver.succeed("sed -i 's|^|command=\"sleep 600\" |' /home/git/.ssh/authorized_keys")
        t = time.time()
        machine.fail("torq-broker push")
        assert time.time() - t < 200, "stopped by the broker's time limit"
        assert "longer than" in last()["error"], last()
        gitserver.succeed("sed -i 's|^command=\"sleep 600\" ||' /home/git/.ssh/authorized_keys")
        machine.succeed("torq-broker push")
        assert last()["ok"] and remote_ledger() == local_ledger(), last()


    with subtest("a torn line never reaches the remote: mended before the push, the entry saying so"):
        machine.succeed("printf '{\"seq\":99,\"cut' >> /var/lib/torq/state/ledger.jsonl")
        machine.succeed("torq-broker push")
        got = remote_ledger()
        assert "\"seq\":99" not in got and got.endswith("\n"), got[-200:]
        assert json.loads(got.splitlines()[-1])["kind"] == "ledger_mended", got[-300:]
        machine.succeed("torq-broker verify")

    with subtest("a main of another history on the remote does not keep the ledger from it"):
        # (As a repository created with a README would have.)
        as_git = lambda cmd: gitserver.succeed(f"su - git -c {shlex.quote(cmd)}")
        as_git("rm -rf other && git init -q other && git -C other -c user.name=x -c user.email=x@x commit -q --allow-empty -m other && git -C other push -q ~/config.git HEAD:refs/heads/other")
        as_git("git -C config.git update-ref refs/heads/main refs/heads/other")
        change(5)
        assert last()["ok"] and "did not take" in last()["main"], last()
        assert remote_ledger() == local_ledger(), "the ledger reached it all the same"
        out = machine.fail("torq-broker status")
        assert "the remote's main did not take" in out, out
        # Put right on the remote's side, the next change pushes main again.
        as_git("git -C config.git update-ref -d refs/heads/main")
        change(6)
        assert last()["ok"] and last().get("main") is None, last()
        machine.succeed("torq-broker status")

    with subtest("a pull loads what waits on the remote into a candidate, and applies nothing"):
        head, entries = machine.succeed(f"git -C {repo} rev-parse HEAD").strip(), local_ledger()
        as_git("rm -rf work && git clone -q ~/config.git work && cd work && git checkout -q -b incoming origin/main"
               " && echo '{\"n\": 99}' > desktop.json && git -c user.name=x -c user.email=x@x commit -qam incoming && git push -q origin incoming")
        incoming = as_git("git -C config.git rev-parse refs/heads/incoming").strip()
        # Fetched — by a person, over the socket: a read — and nothing on the machine changes.
        out = machine.succeed("su - dev -c 'torqcli request system pull'")
        assert "Loaded the remote's incoming configuration" in out and "Kept as this machine's own" in out, out
        assert machine.succeed(f"git -C {repo} rev-parse refs/torq/incoming").strip() == incoming
        assert machine.succeed(f"git -C {repo} rev-parse HEAD").strip() == head, "nothing applied"
        assert local_ledger() == entries, "nothing ledgered"
        machine.fail(f"git -C {repo} show HEAD:desktop.json | grep -q 99")
    with subtest("set beside the history in a merge, by root: an alarm"):
        machine.succeed("cp /var/lib/torq/state/ledger.jsonl /tmp/ledger.good")
        machine.succeed("head -n -2 /tmp/ledger.good > /var/lib/torq/state/ledger.jsonl")
        # A fake root holding the cut ledger, merged with the remote's real tip: not a rewind, so a
        # remote that refuses rewinds takes it.
        root_pushes(f"fake=$({commit_ledger([])}) && tip=$(git ls-remote {url} refs/heads/torq-ledger | cut -f1) && "
                    f"git fetch -q {url} refs/heads/torq-ledger && "
                    f"m=$({commit_ledger(['$fake', '$tip'])}) && git push -q {url} $m:refs/heads/torq-ledger")
        out = machine.fail("torq-broker push")
        assert "merge" in out, out
        out = machine.fail("torq-broker status")
        assert "disagree" in out, out
        # And nothing is pulled from a remote whose ledger disagrees with this one's.
        out = machine.fail("su - dev -c 'torqcli request system pull' 2>&1")
        assert "not pulled" in out, out

    with subtest("the alarm stays: a remote out of reach does not clear it; a person does, ledgered"):
        gitserver.succeed("systemctl stop sshd")
        machine.fail("torq-broker push")
        out = machine.fail("torq-broker status")
        assert "disagree" in out, out
        gitserver.succeed("systemctl start sshd")
        gitserver.wait_for_unit("sshd.service")
        e = json.loads(machine.succeed("torq-broker --json remote-clear --approved-by dev"))
        assert e["kind"] == "remote_cleared" and e["approved_by"] == "dev", e
        # Still true (the merge is still there): raised again at the next push.
        out = machine.fail("torq-broker push")
        assert "merge" in out, out

    def fresh(ledger):
        # A new, empty remote, this machine's ledger as given, and the alarm cleared (that is itself
        # an entry, pushed at once: to the empty remote, which takes it).
        gitserver.succeed(f"rm -rf {remote} && su - git -c {shlex.quote(fresh_remote)}")
        machine.succeed(f"cp {ledger} /var/lib/torq/state/ledger.jsonl")
        machine.succeed("torq-broker --json remote-clear --approved-by dev")
        assert last()["ok"] and remote_ledger() == local_ledger(), last()

    with subtest("cut short here: not pushed, an alarm"):
        fresh("/tmp/ledger.good")
        machine.succeed("cp /var/lib/torq/state/ledger.jsonl /tmp/ledger.good2")
        machine.succeed("head -n -2 /tmp/ledger.good2 > /var/lib/torq/state/ledger.jsonl")
        machine.succeed("torq-broker verify")  # the chain alone cannot see a cut end (KNOWN-GAPS 2)
        before = remote_ledger()
        out = machine.fail("torq-broker push")
        assert "not pushed" in out and "missing here" in out, out
        assert remote_ledger() == before
        out = machine.fail("torq-broker status")
        assert "disagree" in out, out

    with subtest("rewritten here and pushed on top by root: the remote's history says so"):
        fresh("/tmp/ledger.good2")
        machine.succeed("cp /var/lib/torq/state/ledger.jsonl /tmp/ledger.good3")
        machine.succeed("head -n -2 /tmp/ledger.good3 > /var/lib/torq/state/ledger.jsonl")
        root_pushes(f"tip=$(git ls-remote {url} refs/heads/torq-ledger | cut -f1) && git fetch -q {url} refs/heads/torq-ledger && "
                    f"c=$({commit_ledger(['$tip'])}) && git push -q {url} $c:refs/heads/torq-ledger")
        assert remote_ledger() == local_ledger(), "the remote's newest is the rewritten one"
        out = machine.fail("torq-broker push")
        assert "rewritten" in out, out
        out = machine.fail("torq-broker status")
        assert "disagree" in out, out

    with subtest("the alarm is named where a person looks, and stays while the remote agrees again"):
        out = machine.succeed("su - dev -c 'torqcli show system alarms' || true")
        assert "copy on its remote disagree" in out, out
        # Put back as the remote has it: they agree now, and still nothing goes until a person clears it.
        machine.succeed("cp /tmp/ledger.good3 /var/lib/torq/state/ledger.jsonl")
        before = remote_ledger()
        out = machine.fail("torq-broker push")
        assert "not pushed" in out, out
        assert remote_ledger() == before

    with subtest("the remote's ledger gone: an alarm, not a fresh start"):
        fresh("/tmp/ledger.good3")
        remote_git("update-ref -d refs/heads/torq-ledger")
        out = machine.fail("torq-broker push")
        assert "is gone" in out, out

    with subtest("a host key not the one pinned: an alarm, not out of reach"):
        fresh("/tmp/ledger.good3")
        gitserver.succeed("rm -f /etc/ssh/ssh_host_* && ssh-keygen -A && systemctl restart sshd")
        gitserver.wait_for_unit("sshd.service")
        out = machine.fail("torq-broker push")
        assert "host key" in out, out
        # A person decides it is the new key: forgets the old pin, then clears the alarm (in fresh()).
        machine.succeed("rm -f /var/lib/torq/state/remote/known_hosts /var/lib/torq/state/remote/*/known_hosts")

    with subtest("a day without a push is an alarm; late is not"):
        fresh("/tmp/ledger.good3")
        f = "$(ls /var/lib/torq/state/remote/*/last.json)"
        machine.succeed(f"jq '.ok = false | .error = \"out of reach\" | .failing_since = \"2026-01-01T00:00:00Z\"' {f} > /tmp/l && cp /tmp/l {f}")
        out = machine.fail("torq-broker status")
        assert "for a day or more" in out, out
        machine.succeed("torq-broker push && torq-broker status")

    with subtest("main is not pushed while a commit confirmed waits: it may yet be rolled back"):
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        diff = f"--- a/desktop.json\n+++ b/desktop.json\n@@ -1 +1 @@\n-{machine.succeed(f'cat {repo}/desktop.json').strip()}\n+{{\"n\": 77}}\n"
        machine.succeed(f"printf %s {shlex.quote(diff)} > /tmp/c.diff")
        # A test VM boots with no system profile: give it the one a machine has.
        machine.succeed("nix-env -p /nix/var/nix/profiles/system --set $(readlink -f /run/current-system)")
        p = json.loads(machine.succeed(f"torq-broker --json propose --actor user --base {head} --diff /tmp/c.diff --reason 'confirmed'"))
        machine.succeed(f"torq-broker --json apply {p['seq']} --approved-by dev --confirm 30")
        machine.succeed("torq-broker push")
        assert "commit confirmed waits" in last()["main"], last()
        assert remote_git("rev-parse refs/heads/main").strip() == head, "the remote's main is the settled one"
        machine.succeed("torq-broker --json confirm --approved-by dev")
        machine.succeed("torq-broker push")
        assert remote_git("rev-parse refs/heads/main").strip() == machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
  '';
}
