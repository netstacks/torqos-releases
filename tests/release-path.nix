# NixOS VM test: what a changed flake.lock builds is what it names (the audit of 2026-11-04, A2). The
# broker, before it builds a commit that changes the lock, fetches each changed input again by its
# rev and compares the hash, and checks TorqOS's own releases are signed by the release key. Here
# the releases are a repository on the machine, signed with a test key.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-release-path";

  nodes.machine = { pkgs, lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPasswordFile = lib.mkForce null;
    users.users.dev.hashedPassword = "!";
    users.allowNoPasswordLogin = true;
    torq.broker.rebuildCommand = "true";
    # The test's releases: a repository here, and its own key.
    environment.etc."torq/releases.json".text = lib.mkForce (builtins.toJSON { url = "git+file:///srv/releases?ref=stable"; });
    environment.etc."torq/release-signers".source = lib.mkForce "/etc/torq-test/signers";
    environment.systemPackages = [ pkgs.jq pkgs.git ];
    nix.settings.experimental-features = [ "nix-command" "flakes" ];
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    repo = "/var/lib/torq/config"
    machine.succeed(
        "mkdir -p /etc/torq-test /srv && ssh-keygen -q -t ed25519 -N ''' -f /root/rel && "
        "echo \"torqos-release $(cat /root/rel.pub)\" > /etc/torq-test/signers && "
        "ssh-keygen -q -t ed25519 -N ''' -f /root/other"
    )
    g = "git -C /srv/releases -c user.name=r -c user.email=r@r -c gpg.format=ssh"
    machine.succeed(
        f"git init -q -b stable /srv/releases && echo one > /srv/releases/a && {g} add a && "
        f"{g} -c user.signingkey=/root/rel commit -q -S -m one && "
        f"echo two > /srv/releases/a && {g} commit -qam two && "  # not signed
        f"echo three > /srv/releases/a && {g} -c user.signingkey=/root/other commit -qam three -S"  # another key
    )
    one, two, three = machine.succeed("git -C /srv/releases rev-list --reverse stable").split()
    machine.succeed(f"mkdir -p {repo} && echo '{{}}' > {repo}/desktop.json && touch {repo}/flake.nix && echo '{{}}' > {repo}/flake.lock && torq-broker init")

    def nar(rev):
        return json.loads(machine.succeed(f"nix eval --impure --json --expr '(builtins.fetchTree {{ type = \"git\"; url = \"file:///srv/releases\"; rev = \"{rev}\"; }}).narHash'"))

    def lock(rev, nar_hash):
        return json.dumps({"nodes": {"root": {"inputs": {"torq": "torq"}}, "torq": {"locked": {"type": "git", "url": "file:///srv/releases", "rev": rev, "narHash": nar_hash, "lastModified": 1}}}, "root": "root", "version": 7}, indent=2)

    def apply(text):
        machine.succeed(f"rm -rf /tmp/c && cp -r {repo} /tmp/c && printf '%s\\n' {shlex.quote(text)} > /tmp/c/flake.lock && cd /tmp/c && git diff > /tmp/l.diff")
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        p = json.loads(machine.succeed(f"torq-broker --json propose --base {head} --diff /tmp/l.diff --reason lock"))
        return machine.execute(f"torq-broker --json apply {p['seq']} --approved-by dev 2>&1")

    with subtest("a release signed by the release key, its hash what its rev fetches: applied"):
        code, out = apply(lock(one, nar(one)))
        assert code == 0, out

    with subtest("the signed rev, and the hash of other source: refused, nothing applied"):
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        code, out = apply(lock(one, nar(three)))
        assert code != 0 and "not what its rev fetches" in out, out
        assert machine.succeed(f"git -C {repo} rev-parse HEAD").strip() == head

    with subtest("a release not signed, or signed by another key: refused"):
        for rev in [two, three]:
            code, out = apply(lock(rev, nar(rev)))
            assert code != 0 and "not signed by the TorqOS release key" in out, (rev, out)
        machine.succeed("torq-broker verify && torq-broker status")
  '';
}
