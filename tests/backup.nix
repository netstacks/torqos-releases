# NixOS VM test: the declared backup (ADR-038). NixOS's own restic backup, to a repository on the
# machine's own disk; its key given at the terminal and kept root's alone, never in the repository or
# the ledger; the machine's secrets never in the backup; a file deleted and brought back, written as the
# owner of its folder — so a link the person planted leads nowhere they could not go — what was there
# set aside; an administrator's, with their password, every restore in the ledger.
{ pkgs, self }:

let
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
in
pkgs.testers.runNixOSTest {
  name = "torq-backup";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    users.users.bob = { isNormalUser = true; group = "users"; };
    users.users.carol = { isNormalUser = true; group = "users"; };
    users.users.svc = { isSystemUser = true; group = "svc"; };
    users.groups.svc = { };
    torq.broker.rebuildCommand = "true";
    # As the tree would set it: where it goes, and nothing of its key (modules/backup.nix: its
    # passwordFile, /home and /var/lib). By hand here, not on a timer.
    services.restic.backups.local = {
      repository = "/srv/restic";
      initialize = true;
      timerConfig = null;
    };
    environment.systemPackages = [ pkgs.jq pkgs.tmux pkgs.git ];
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    pw = "Torq-sock-2210"
    key = "Backup-passphrase-7731"
    repo = "/var/lib/torq/config"
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")

    def as_user(user, cmd):
        return f"su - {user} -c {shlex.quote(cmd)}"

    def last():
        return json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))

    def at_terminal(name, line, answers):
        """As a person at torqcli: in a terminal, each prompt answered as it comes; its exit and screen."""
        machine.succeed("rm -f /tmp/rc")
        machine.succeed(as_user("dev", f"tmux new-session -d -s {name} -x 200 -y 50 {shlex.quote(f'torqcli {line}; echo done=$? > /tmp/rc; sleep 600')}"))
        for prompt, answer in answers:
            machine.wait_until_succeeds(as_user("dev", f"tmux capture-pane -p -t {name}") + f" | grep -q {shlex.quote(prompt)}", timeout=60)
            machine.succeed(as_user("dev", f"tmux send-keys -t {name} {shlex.quote(answer)} Enter"))
        machine.wait_until_succeeds("test -e /tmp/rc", timeout=300)
        out = machine.succeed(as_user("dev", f"tmux capture-pane -p -t {name}"))
        machine.succeed(as_user("dev", f"tmux kill-session -t {name}"))
        return machine.succeed("cat /tmp/rc").strip(), " ".join(out.split())

    def restore(name, args):
        return at_terminal(name, f"request system restore local {args}", [("assword for dev", pw)])

    with subtest("declared as NixOS's own, its key pointed outside the repository"):
        settings = json.loads(machine.succeed("cat /etc/torq/broker.json"))
        assert list(settings["backups"]) == ["local"], settings["backups"]
        unit = machine.succeed("systemctl cat restic-backups-local.service")
        assert "RESTIC_PASSWORD_FILE=/var/lib/torq/secrets/backup/local" in unit, unit
        out = machine.succeed(as_user("dev", "torqcli show system backups"))
        assert "local" in out and "request system backup key local" in out, out

    with subtest("the key, typed at the terminal: root's alone, and nowhere else"):
        rc, out = at_terminal("k", "request system backup key local", [("Key of backup local", key), ("Again", key), ("assword for dev", pw)])
        assert rc == "done=0", out
        assert machine.succeed("stat -c %U:%a /var/lib/torq/secrets/backup /var/lib/torq/secrets/backup/local").split() == ["root:700", "root:600"]
        assert machine.succeed("cat /var/lib/torq/secrets/backup/local") == key
        machine.fail(as_user("bob", "cat /var/lib/torq/secrets/backup/local"))
        machine.fail(as_user("dev", "cat /var/lib/torq/secrets/backup/local"))
        e = last()
        # (The socket adds who sent it, as it does to every entry; never the key.)
        assert e["kind"] == "backup_key" and e["approved_by"] == "dev" and e["data"]["backup"] == "local" and set(e["data"]) <= {"backup", "via", "caller"}, e
        # Not in the ledger, the repository and its history, or anything a request left behind.
        machine.fail(f"grep -rF {key} /var/lib/torq/state {repo}")
        machine.fail(f"git -C {repo} log -p --all | grep -F {key}")
        # Not a person's to give without being an administrator, nor the agent's.
        out = machine.fail("su -s /bin/sh torq-agent -c 'torqcli request system backup key local' 2>&1 < /dev/null")
        assert "terminal" in out or "a person applies" in out, out

    with subtest("a backup of /home and /var/lib, without the machine's secrets"):
        machine.succeed(as_user("bob", "mkdir -p docs && echo one > docs/a && echo keep > docs/b"))
        machine.succeed(as_user("bob", "mkdir -p trap && echo ok > trap/f"))
        machine.succeed(as_user("bob", "mkdir -p tree && mkfifo tree/pipe && cp /run/current-system/sw/bin/id tree/sid && chmod 4755 tree/sid"))
        machine.succeed(as_user("bob", "mkdir -p devs && echo keep > devs/f") + " && mknod /home/bob/devs/z c 1 3")
        # A home the repository says is bob's, where the machine says carol's is.
        machine.succeed("echo c > /home/carol/c && chown -R bob /home/carol")
        machine.succeed("mkdir -p /var/lib/svcdata && echo v1 > /var/lib/svcdata/db && chown -R svc:svc /var/lib/svcdata && chmod 750 /var/lib/svcdata")
        machine.succeed("systemctl start restic-backups-local.service")
        listed = machine.succeed("restic-local ls latest")
        assert "/home/bob/docs/a" in listed and "/var/lib/svcdata/db" in listed, listed
        assert "/var/lib/torq/secrets" not in listed, "the keys and password hashes are not in a backup"
        assert "/var/lib/torq/state/ledger.jsonl" in listed, "the rest of /var/lib is"
        out = machine.succeed(as_user("dev", "torqcli show system backups"))
        # No timer here, so systemd forgets the run: the newest snapshot is said instead.
        assert ("last run" in out and "done" in out or "last snapshot" in out) and "/home /var/lib" in out, out
        # Everyone may look; the agent too: what is listed is no secret.
        machine.succeed(as_user("bob", "torqcli show system backups"))

    with subtest("a deleted file comes back, as its owner, and what was there is kept"):
        machine.succeed(as_user("bob", "rm docs/a && echo two > docs/b"))
        rc, out = restore("r1", "/home/bob/docs")
        assert rc == "done=0", out
        assert machine.succeed("cat /home/bob/docs/a").strip() == "one"
        assert machine.succeed("cat /home/bob/docs/b").strip() == "keep"
        assert machine.succeed("stat -c %U /home/bob/docs /home/bob/docs/a").split() == ["bob", "bob"]
        e = last()
        assert e["kind"] == "backup_restored" and e["approved_by"] == "dev" and e["data"]["path"] == "/home/bob/docs", e
        assert len(e["data"]["backup_snapshot"]) == 64 and e["data"]["error"] is None, e
        assert machine.succeed(f"cat {e['data']['set_aside']}/b").strip() == "two"
        # Nothing of it left where root put it.
        machine.succeed("test -z \"$(ls -A /var/cache/torq/restoring)\" && stat -c %U:%a /var/cache/torq/restoring | grep -qx root:700")

    with subtest("by its snapshot's id; a service's whole folder, in a folder of root's"):
        snap = json.loads(machine.succeed("torq-broker --json backups"))[0]["snapshots"][-1]["id"][:8]
        # Its contents lost, the folder as its service keeps it: the owner is the machine's word, never
        # the repository's (the review of the backup, H1). A folder gone altogether is refused, below.
        machine.succeed("rm -rf /var/lib/svcdata/* && echo damaged > /var/lib/svcdata/db && chown svc:svc /var/lib/svcdata/db")
        rc, out = restore("r2", f"{snap} /var/lib/svcdata")
        assert rc == "done=0", out
        assert machine.succeed("stat -c %U:%a /var/lib/svcdata").strip() == "svc:750"
        assert machine.succeed("cat /var/lib/svcdata/db").strip() == "v1"

    with subtest("a planted link goes nowhere its owner could not go"):
        machine.succeed(as_user("bob", "mv trap trap2 && ln -s /etc trap"))
        before = machine.succeed("sha256sum /etc/shadow /etc/passwd")
        rc, out = restore("r3", "/home/bob/trap/f")
        assert rc != "done=0", out
        assert machine.succeed("sha256sum /etc/shadow /etc/passwd") == before, "root's files untouched"
        machine.fail("test -e /etc/f")
        machine.fail("ls /etc/f.restoring-*")
        assert machine.succeed("readlink /home/bob/trap").strip() == "/etc"
        e = last()
        assert e["kind"] == "backup_restored" and e["data"]["error"], e

    with subtest("set-id bits do not come back, a pipe does, a device fails it"):
        machine.succeed(as_user("bob", "rm -rf tree"))
        rc, out = restore("r4", "/home/bob/tree")
        assert rc == "done=0", out
        assert machine.succeed("stat -c %a:%U /home/bob/tree/sid").strip() == "755:bob"
        machine.succeed("test -p /home/bob/tree/pipe")
        # restic's restore makes no device (no CAP_MKNOD, PrivateDevices): it fails, what is live stays.
        machine.succeed(as_user("bob", "echo live > devs/f"))
        rc, out = restore("r5", "/home/bob/devs")
        assert rc != "done=0", out
        assert machine.succeed("cat /home/bob/devs/f").strip() == "live"
        machine.fail("ls -d /home/bob/devs.restoring-* /home/bob/devs.before-*")
        assert last()["data"]["error"], last()

    with subtest("whose a folder is, the machine says, never the repository"):
        # The repository says bob's; carol's account says hers: refused, nothing made.
        machine.succeed("rm -rf /home/carol")
        rc, out = restore("r6", "/home/carol")
        assert rc != "done=0" and "in the backup and uid" in out, out
        machine.fail("test -e /home/carol")
        machine.fail("ls -d /home/carol.restoring-*")
        # A service's folder comes back only into one the machine has.
        out = machine.fail("torq-broker backup-restore --name local --path /var/lib/not-here --approved-by dev 2>&1")
        assert "not on this machine" in out, out

    with subtest("what may not be restored, and by whom"):
        for path, why in [("/var/lib/torq/secrets/backup/local", "machine's own"), ("/var/lib/torq/state/ledger.jsonl", "machine's own"), ("/etc/shadow", "below /home and /var/lib"), ("/home/bob/*", "pattern")]:
            out = machine.fail(f"torq-broker backup-restore --name local --path {shlex.quote(path)} --approved-by dev 2>&1")
            assert why in out, (path, out)
        out = machine.fail(as_user("bob", "torqcli request system restore local /home/bob/docs") + " 2>&1")
        assert "not an administrator" in out, out
        out = machine.fail("su -s /bin/sh torq-agent -c 'torqcli request system restore local /home/bob/docs' 2>&1")
        assert "a person applies" in out, out

    with subtest("in the tree: where it goes is a setting, its key is not"):
        out = machine.succeed(as_user("dev", "torqcli -c 'configure; set services restic backups nas repository sftp:backup@nas:/torq; show | compare'"))
        assert "sftp:backup@nas:/torq" in out, out
        out = machine.fail(as_user("dev", "torqcli -c 'configure; set services restic backups nas passwordFile /home/dev/k'") + " 2>&1")
        assert "a backup's key is never in the configuration" in out, out
        # And the passphrase was never written anywhere a person can read.
        machine.fail(f"grep -rF {key} /home /etc {repo} /var/lib/torq/state")
  '';
}
