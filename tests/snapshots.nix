# NixOS VM test: snapshots of the data (ADR-038). A btrfs disk laid out as the image's, its subvolumes
# mounted at boot. A snapshot before each system change, read only and root's alone;
# the ledger never in one; a restore that runs as the folder's owner, so a link a person planted
# leads nowhere they could not go; what was there kept; every restore in the ledger.
{ pkgs, self }:

let
  devHash = "$6$torqsocketsalt$Z7RLey3l5vKGfukhfuZYYM/EjWNo7QM8.65r88XmZ9z3/jBCY.NAnH7RwYpiLoE2fedxUd/I7UFcKDX1KEXDg/";
in
pkgs.testers.runNixOSTest {
  name = "torq-snapshots";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = devHash;
    users.users.bob = { isNormalUser = true; group = "users"; };
    users.users.svc = { isSystemUser = true; group = "svc"; };
    users.groups.svc = { };
    torq.broker.rebuildCommand = "true";
    torq.snapshots.keep = 3;
    # The image's layout, mounted at boot as an installed machine's is, so snapshots come on by
    # themselves. A test VM's disk starts empty: laid out in the initrd, before anything mounts it.
    # The data, and the record's own, small: so that the first fills while the second has room.
    virtualisation.emptyDiskImages = [ 2048 64 ];
    virtualisation.fileSystems =
      let
        subvolume = name: { device = "/dev/vdb"; fsType = "btrfs"; options = [ "subvol=${name}" ]; };
      in
      {
        "/home" = subvolume "@home" // { neededForBoot = true; };
        "/var/lib" = subvolume "@var-lib" // { neededForBoot = true; };
        "/var/lib/torq" = { device = "/dev/vdc"; fsType = "ext4"; neededForBoot = true; };
        "/.snapshots" = subvolume "@snapshots";
      };
    boot.initrd.systemd.initrdBin = [ pkgs.btrfs-progs pkgs.e2fsprogs ];
    boot.initrd.systemd.services.lay-out-disk = {
      unitConfig.DefaultDependencies = false;
      requires = [ "dev-vdb.device" "dev-vdc.device" ];
      after = [ "dev-vdb.device" "dev-vdc.device" ];
      before = [ "initrd-fs.target" "sysroot-home.mount" "sysroot-var-lib.mount" "sysroot-var-lib-torq.mount" "systemd-fsck@dev-vdc.service" ];
      requiredBy = [ "sysroot-home.mount" "sysroot-var-lib.mount" "sysroot-var-lib-torq.mount" "systemd-fsck@dev-vdc.service" ];
      # Once: the initrd reloads systemd on its way, and a finished one-shot would run again.
      serviceConfig = { Type = "oneshot"; RemainAfterExit = true; };
      script = ''
        mkfs.btrfs -q -L data /dev/vdb
        mkfs.ext4 -q -L torq /dev/vdc
        mkdir -p /top
        mount -t btrfs /dev/vdb /top
        for s in @home @var-lib @snapshots; do btrfs -q subvolume create /top/$s; done
        chmod 700 /top/@snapshots
        umount /top
      '';
    };
    environment.systemPackages = [ pkgs.jq pkgs.tmux pkgs.btrfs-progs pkgs.git ];
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    pw = "Torq-sock-2210"
    repo = "/var/lib/torq/config"

    with subtest("a disk laid out as the image's"):
        machine.succeed("mountpoint -q /var/lib/torq && mountpoint -q /var/lib && mountpoint -q /home && mountpoint -q /.snapshots")
        machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && torq-broker init")

    def broker(args):
        return json.loads(machine.succeed(f"torq-broker --json {args}").strip().splitlines()[-1])

    def change(n):
        machine.succeed(f"cd {repo} && rm -rf /tmp/c{n} && cp -r . /tmp/c{n} && cd /tmp/c{n} && echo '{{\"n\": {n}}}' > desktop.json && git diff > /tmp/c{n}.diff")
        p = broker(f"propose --diff /tmp/c{n}.diff --reason change-{n}")["seq"]
        return broker(f"apply {p} --approved-by dev")

    def as_user(user, cmd):
        return f"su - {user} -c {shlex.quote(cmd)}"

    def restore(n, path):
        """As a person at torqcli would: over the socket, with the password."""
        machine.succeed(as_user("dev", f"tmux new-session -d -s r{n} -x 170 -y 40 {shlex.quote(f'torqcli request system snapshot restore {n} {path}; echo done=$? > /tmp/rc; sleep 600')}"))
        machine.wait_until_succeeds(as_user("dev", f"tmux capture-pane -p -t r{n}") + " | grep -q 'assword'", timeout=60)
        machine.succeed("rm -f /tmp/rc")
        machine.succeed(as_user("dev", f"tmux send-keys -t r{n} {pw} Enter"))
        machine.wait_until_succeeds("test -e /tmp/rc", timeout=60)
        out = machine.succeed(as_user("dev", f"tmux capture-pane -p -t r{n}"))
        machine.succeed(as_user("dev", f"tmux kill-session -t r{n}"))
        return machine.succeed("cat /tmp/rc").strip(), " ".join(out.split())

    with subtest("a snapshot before each change: read only, root's, without the ledger"):
        machine.succeed(as_user("bob", "mkdir -p docs && echo one > docs/a && echo keep > docs/b"))
        machine.succeed("mkdir -p /var/lib/svcdata && echo v1 > /var/lib/svcdata/db && chown -R svc:svc /var/lib/svcdata")
        a = change(1)
        label = a["data"]["snapshot"]["label"]
        machine.succeed(f"btrfs property get -ts /.snapshots/{label}/home ro | grep -q true")
        machine.succeed(f"grep -q one /.snapshots/{label}/home/bob/docs/a")
        machine.fail(f"touch /.snapshots/{label}/home/x")
        machine.fail(as_user("bob", f"ls /.snapshots/{label}"))
        machine.fail(as_user("bob", "ls /.snapshots"))
        # The record is a subvolume of its own: not in a snapshot of /var/lib, so never rewound.
        machine.fail(f"test -e /.snapshots/{label}/var-lib/torq/state/ledger.jsonl")
        out = machine.succeed(as_user("dev", "torqcli show system snapshots"))
        assert f"#{a['seq']}" in out and "change-1" in out, out

    with subtest("a path comes back, as its owner, and what was there is kept"):
        machine.succeed(as_user("bob", "echo two > docs/a && rm docs/b"))
        rc, out = restore(a["seq"], "/home/bob/docs")
        assert rc == "done=0", out
        assert machine.succeed("cat /home/bob/docs/a").strip() == "one"
        assert machine.succeed("cat /home/bob/docs/b").strip() == "keep"
        assert machine.succeed("stat -c %U /home/bob/docs/a").strip() == "bob"
        assert machine.succeed(f"cat /home/bob/docs.before-{label}/a").strip() == "two"
        e = json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))
        assert e["kind"] == "data_restored" and e["approved_by"] == "dev" and e["data"]["path"] == "/home/bob/docs", e

    with subtest("a service's whole folder comes back, in a folder of root's"):
        machine.succeed("echo v2-migrated > /var/lib/svcdata/db")
        rc, out = restore(a["seq"], "/var/lib/svcdata")
        assert rc == "done=0", out
        assert machine.succeed("cat /var/lib/svcdata/db").strip() == "v1"
        assert machine.succeed("stat -c %U:%a /var/lib/svcdata").strip().startswith("svc:")
        assert machine.succeed(f"cat /var/lib/svcdata.before-{label}/db").strip() == "v2-migrated"

    def last():
        return json.loads(machine.succeed("torq-broker log --json --brief | tail -1"))

    with subtest("a planted link goes nowhere its owner could not go"):
        # In the snapshot: refused. Live, where the path is to be written: followed as bob, not root.
        machine.succeed(as_user("bob", "mkdir -p trap && echo ok > trap/f && ln -s /etc trap/etc"))
        b = change(2)
        rc, out = restore(b["seq"], "/home/bob/trap/etc/shadow")
        assert rc != "done=0" and "is a link in the snapshot" in out, out
        machine.succeed(as_user("bob", "mv trap trap2 && ln -s /etc trap"))
        before = machine.succeed("sha256sum /etc/shadow /etc/passwd")
        rc, out = restore(b["seq"], "/home/bob/trap/f")
        assert rc != "done=0", out
        assert machine.succeed("sha256sum /etc/shadow /etc/passwd") == before, "root's files untouched"
        machine.fail("test -e /etc/f")
        # It failed, and the ledger says so, with what bob's link was put back as it stood.
        e = last()
        assert e["kind"] == "data_restored" and e["data"]["error"], e
        assert machine.succeed("readlink /home/bob/trap").strip() == "/etc"

    with subtest("the owner holds a pipe, nothing of the snapshot; and stopping it stops no one else"):
        machine.succeed(as_user("bob", "mkdir -p big && dd if=/dev/urandom of=big/x bs=1M count=300 status=none"))
        c = change(6)
        machine.succeed(as_user("bob", "rm -rf big"))
        machine.succeed(as_user("dev", f"tmux new-session -d -s stall -x 170 -y 40 {shlex.quote(f'torqcli request system snapshot restore {c['seq']} /home/bob/big; echo done=$? > /tmp/rc-stall; sleep 600')}"))
        machine.wait_until_succeeds(as_user("dev", "tmux capture-pane -p -t stall") + " | grep -q 'assword'", timeout=60)
        machine.succeed(as_user("dev", f"tmux send-keys -t stall {pw} Enter"))
        # In a tight loop in the machine: a copy can be over between two of the driver's polls.
        machine.succeed("timeout 60 sh -c 'until pkill -STOP -u bob -x tar; do sleep 0.01; done'")
        # Every descriptor and folder of every process of the owner's, while it is held still.
        held = machine.succeed("for p in $(pgrep -u bob); do readlink /proc/$p/fd/* /proc/$p/cwd; done").split()
        assert any(h.startswith("pipe:") for h in held), held
        assert not any("/.snapshots" in h for h in held), held
        # bob's process is stopped, in the middle of a restore: a change goes through all the same.
        change(60)
        machine.succeed("pkill -CONT -u bob -x tar")
        machine.wait_until_succeeds("test \"$(cat /tmp/rc-stall)\" = done=0", timeout=120)
        machine.succeed("test $(stat -c %s /home/bob/big/x) = 314572800")
        machine.fail("ls -d /home/bob/big.restoring-*")

    with subtest("what appears during a copy is not overwritten"):
        machine.succeed("mkdir -p /var/lib/svcbig && dd if=/dev/urandom of=/var/lib/svcbig/x bs=1M count=200 status=none && chown -R svc:svc /var/lib/svcbig")
        w = change(63)
        machine.succeed("rm -rf /var/lib/svcbig")
        machine.succeed(as_user("dev", f"tmux new-session -d -s race -x 170 -y 40 {shlex.quote(f'torqcli request system snapshot restore {w['seq']} /var/lib/svcbig; echo done=$? > /tmp/rc-race; sleep 600')}"))
        machine.wait_until_succeeds(as_user("dev", "tmux capture-pane -p -t race") + " | grep -q 'assword'", timeout=60)
        machine.succeed(as_user("dev", f"tmux send-keys -t race {pw} Enter"))
        machine.succeed("timeout 60 sh -c 'until pkill -STOP -u svc -x tar; do sleep 0.01; done'")
        # Something of the service's starts up again while the copy waits, and writes its folder.
        machine.succeed("mkdir /var/lib/svcbig && echo fresh > /var/lib/svcbig/new && chown -R svc:svc /var/lib/svcbig")
        machine.succeed("pkill -CONT -u svc -x tar")
        machine.wait_until_succeeds("grep -q done= /tmp/rc-race", timeout=120)
        # It was set aside, whole, and the snapshot's is in its place: nothing lost either way.
        assert machine.succeed("cat /tmp/rc-race").strip() == "done=0"
        e = last()
        assert e["data"]["set_aside"] and machine.succeed(f"cat {e['data']['set_aside']}/new").strip() == "fresh", e
        machine.succeed("test $(stat -c %s /var/lib/svcbig/x) = 209715200")

    with subtest("a failed copy leaves what is live as it was, whatever it stopped on"):
        # A device in the folder: tar cannot make one as bob. A read-only folder before it: what
        # was staged has to be got rid of all the same.
        machine.succeed(as_user("bob", "mkdir -p mixed/ro && echo old > mixed/keep && echo a > mixed/ro/a && chmod 555 mixed/ro"))
        machine.succeed("mknod /home/bob/mixed/z-dev c 1 3")
        g = change(61)
        machine.succeed("rm /home/bob/mixed/z-dev")
        machine.succeed(as_user("bob", "chmod 755 mixed/ro && echo new > mixed/keep"))
        rc, out = restore(g["seq"], "/home/bob/mixed")
        assert rc != "done=0", out
        assert machine.succeed("cat /home/bob/mixed/keep").strip() == "new", "what is live did not change"
        machine.fail("ls -d /home/bob/mixed.restoring-* /home/bob/mixed.before-*")
        assert last()["data"]["error"], last()

    with subtest("in a folder: a pipe comes back as one, set-id bits do not, and a secret stays one"):
        machine.succeed(as_user("bob", "mkdir -p tree && mkfifo tree/pipe && cp /run/current-system/sw/bin/id tree/sid && chmod 4755 tree/sid"))
        machine.succeed(as_user("bob", "mkdir -p -m 700 vault/in && echo s > vault/in/key && chmod 600 vault/in/key"))
        h = change(62)
        machine.succeed(as_user("bob", "rm -rf tree vault"))
        rc, out = restore(h["seq"], "/home/bob/tree")
        assert rc == "done=0", out
        assert machine.succeed("stat -c %F:%a /home/bob/tree/pipe /home/bob/tree/sid").split("\n")[1].endswith(":755")
        machine.succeed("test -p /home/bob/tree/pipe")
        rc, out = restore(h["seq"], "/home/bob/vault/in/key")
        assert rc == "done=0", out
        assert machine.succeed("stat -c %a /home/bob/vault /home/bob/vault/in /home/bob/vault/in/key").split() == ["700", "700", "600"]

    with subtest("what would hang, what is gone, what stands alone"):
        machine.succeed(as_user("bob", "mkfifo pipe"))
        machine.succeed("echo row > /var/lib/alone.db && chown svc:svc /var/lib/alone.db && chmod 640 /var/lib/alone.db")
        machine.succeed("mkdir -p /var/lib/gone && echo there > /var/lib/gone/f && chown -R svc:svc /var/lib/gone && chmod 750 /var/lib/gone")
        # bob once kept a program set-group-id, in a group he has since left.
        machine.succeed("cp /run/current-system/sw/bin/id /home/bob/sid && chown bob:wheel /home/bob/sid && chmod 2755 /home/bob/sid")
        d = change(7)
        rc, out = restore(d["seq"], "/home/bob/pipe")
        assert rc != "done=0" and "neither a file nor a folder" in out, out
        machine.succeed("torq-broker --json status")
        # A folder deleted whole comes back: the main case.
        machine.succeed("rm -rf /var/lib/gone")
        rc, out = restore(d["seq"], "/var/lib/gone")
        assert rc == "done=0", out
        assert machine.succeed("stat -c %U:%a /var/lib/gone").strip() == "svc:750"
        assert machine.succeed("cat /var/lib/gone/f").strip() == "there"
        assert last()["data"]["set_aside"] is None, last()
        # A file directly under /var/lib: a file again, not a folder at its name.
        machine.succeed("echo damaged > /var/lib/alone.db")
        rc, out = restore(d["seq"], "/var/lib/alone.db")
        assert rc == "done=0", out
        assert machine.succeed("stat -c %F:%U:%a /var/lib/alone.db").strip() == "regular file:svc:640"
        assert machine.succeed("cat /var/lib/alone.db").strip() == "row"
        # And again: its set-aside is in a folder of root's, so a clear refusal, changing nothing.
        rc, out = restore(d["seq"], "/var/lib/alone.db")
        assert rc != "done=0" and "already there" in out, out
        assert machine.succeed("cat /var/lib/alone.db").strip() == "row"
        # Set-id bits never come back, nor the group bob has left: bob's primary group now.
        machine.succeed("rm /home/bob/sid")
        rc, out = restore(d["seq"], "/home/bob/sid")
        assert rc == "done=0", out
        got = machine.succeed("stat -c %a:%G /home/bob/sid").strip()
        assert got == "755:users", got

    with subtest("whose folder it is decides, and root's is not restored"):
        machine.succeed("mkdir -p /var/lib/rootdata && echo r > /var/lib/rootdata/f")
        machine.succeed("mkdir -p /var/lib/moved && chown svc:svc /var/lib/moved")
        f = change(8)
        rc, out = restore(f["seq"], "/var/lib/rootdata")
        assert rc != "done=0" and "folder of root's" in out, out
        machine.succeed("mv /var/lib/moved /var/lib/moved2 && ln -s /etc /var/lib/moved")
        rc, out = restore(f["seq"], "/var/lib/moved")
        assert rc != "done=0" and "changed hands" in out, out
        assert machine.succeed("readlink /var/lib/moved").strip() == "/etc"
        machine.succeed("rm /var/lib/moved && mv /var/lib/moved2 /var/lib/moved && chown bob /var/lib/moved")
        rc, out = restore(f["seq"], "/var/lib/moved")
        assert rc != "done=0" and "changed hands" in out, out

    with subtest("what may not be restored, and by whom"):
        for path, why in [("/var/lib/torq/state/ledger.jsonl", "machine's own"), ("/etc/shadow", "below /home and /var/lib"), ("/home/bob/../../etc", "no `..`")]:
            out = machine.fail(f"torq-broker restore-data {b['seq']} --path {path} --approved-by dev 2>&1")
            assert why in out, (path, out)
        # Not a person's to ask for without being an administrator, nor the agent's.
        out = machine.fail(as_user("bob", f"torqcli request system snapshot restore {b['seq']} /home/bob/docs") + " 2>&1")
        assert "not an administrator" in out, out
        out = machine.fail(f"su -s /bin/sh torq-agent -c 'torqcli request system snapshot restore {b['seq']} /home/bob/docs' 2>&1")
        assert "a person applies" in out, out

    with subtest("by hand: one taken, and one deleted; settings in the tree"):
        def as_person(cmd):
            machine.succeed(as_user("dev", f"tmux new-session -d -s h -x 170 -y 40 {shlex.quote(f'torqcli {cmd}; echo done=$? > /tmp/rc-h; sleep 600')}"))
            machine.wait_until_succeeds(as_user("dev", "tmux capture-pane -p -t h") + " | grep -q 'assword'", timeout=60)
            machine.succeed("rm -f /tmp/rc-h")
            machine.succeed(as_user("dev", f"tmux send-keys -t h {pw} Enter"))
            machine.wait_until_succeeds("test -e /tmp/rc-h", timeout=60)
            machine.succeed(as_user("dev", "tmux kill-session -t h"))
            return machine.succeed("cat /tmp/rc-h").strip()
        assert as_person("request system snapshot take") == "done=0"
        t = last()
        assert t["kind"] == "snapshot_taken" and t["approved_by"] == "dev", t
        lbl = t["data"]["snapshot"]["label"]
        machine.succeed(f"test -d /.snapshots/{lbl}/home")
        assert f"#{t['seq']}" in machine.succeed(as_user("dev", "torqcli show system snapshots"))
        assert as_person(f"request system snapshot delete {t['seq']}") == "done=0"
        machine.fail(f"test -e /.snapshots/{lbl}")
        assert last()["kind"] == "snapshot_deleted", last()
        assert f"#{t['seq']}" not in machine.succeed(as_user("dev", "torqcli show system snapshots"))
        # Not a person's without the group, nor the agent's, and no one's without a password.
        out = machine.fail(as_user("bob", "torqcli request system snapshot take") + " 2>&1")
        assert "not an administrator" in out, out
        out = machine.fail("su -s /bin/sh torq-agent -c 'torqcli request system snapshot delete 1' 2>&1")
        assert "a person applies" in out, out
        # In the tree, as any setting; what the broker runs for it stays out of reach.
        out = machine.succeed(as_user("dev", "torqcli -c 'configure; set torq snapshots keep 5; show | compare'"))
        assert "keep 5" in out, out
        out = machine.fail(as_user("dev", "torqcli -c 'configure; set torq broker rebuildCommand x'") + " 2>&1")
        assert "not settable" in out, out

    with subtest("a disk filled by another account leaves the record room (the audit of M3, M2)"):
        # As the agent, in the home it has: until nothing more goes in.
        machine.succeed("su -s /bin/sh torq-agent -c 'dd if=/dev/zero of=/var/lib/torq-agent/fill bs=1M status=none' || true")
        machine.fail("su -s /bin/sh torq-agent -c 'echo x > /var/lib/torq-agent/more'")
        # The record is written all the same: an entry, and a person's change.
        n = int(machine.succeed("torq-broker log --json --brief | wc -l"))
        machine.succeed("torq-broker break-glass --by dev --reason 'the disk is full'")
        assert last()["kind"] == "break_glass", last()
        assert int(machine.succeed("torq-broker log --json --brief | wc -l")) == n + 1
        machine.succeed("rm /var/lib/torq-agent/fill")

    with subtest("a store path changed in place is found, and said (the audit of M3, M5)"):
        machine.succeed("torq-broker verify-store")
        machine.succeed("torq-broker status")
        p = machine.succeed("readlink -f $(command -v torq-snapshot 2>/dev/null || ls -d /nix/store/*-torq-snapshot | head -1)").strip()
        p = "/".join(p.split("/")[:4])
        machine.succeed("mount -o remount,rw /nix/store 2>&1 || mount -o bind,remount,rw /nix/store")
        machine.succeed(f"chmod u+w {p}/bin/torq-snapshot && echo '# changed' >> {p}/bin/torq-snapshot")
        machine.fail("torq-broker verify-store")
        out = machine.fail("torq-broker status")
        assert "changed after they were built" in out and p in out, out
        out = machine.succeed(as_user("dev", "torqcli show system alarms || true"))
        assert "changed after they were built" in out, out
        machine.succeed(f"sed -i '$d' {p}/bin/torq-snapshot")
        machine.succeed("torq-broker verify-store && torq-broker status")

    with subtest("a person's own changes push nothing out; only the newest are kept"):
        before = machine.succeed("ls /.snapshots")
        machine.succeed("printf -- '--- /dev/null\\n+++ b/users/bob/packages.json\\n@@ -0,0 +1 @@\\n+[\"hello\"]\\n' > /tmp/own.diff")
        p = broker("propose --diff /tmp/own.diff --reason own")["seq"]
        e = broker(f"apply {p} --approved-by bob")
        assert e["tier"] == "user" and "snapshot" not in e["data"], e
        assert machine.succeed("ls /.snapshots") == before
        for n in [10, 11, 12]:
            change(n)
        assert len(machine.succeed("ls /.snapshots").split()) == 3
        machine.fail(f"test -e /.snapshots/{label}")
        out = machine.succeed(as_user("dev", "torqcli show system snapshots"))
        assert "change-1 " not in out and "change-12" in out, out
        machine.succeed("torq-broker verify")
  '';
}
