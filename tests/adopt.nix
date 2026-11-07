# NixOS VM test: a new machine that becomes another (ADR-036, "your machine is a URL"). `first` is set
# up on its console and its repository and ledger are put on a git server, as its broker pushes them;
# `second` boots the same image, chooses a repository at setup, adds its key, reads what it found,
# and adopts it: the same configuration and history, one entry on top, its own password. As in
# tests/image.nix the VM cannot build systems: setup's build is a prebuilt specialisation.
{ pkgs, self }:

let
  image = { lib, ... }: {
    imports = [ self.nixosModules.default ];
    environment.systemPackages = [ pkgs.jq pkgs.git ];
    torq.setup.enable = true;
    torq.setup.source = self.sourceInfo;
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    specialisation.alice.configuration = {
      torq.setup.enable = lib.mkForce false;
      torq.user = "alice";
      networking.hostName = lib.mkForce "first";
    };
    torq.broker.buildCommand = "readlink -f /run/booted-system/specialisation/alice";
    torq.broker.rebuildCommand = "/run/booted-system/specialisation/alice/bin/switch-to-configuration switch";
    torq.broker.bootCommand = "nix-env -p {profile} --set {system}";
    virtualisation.memorySize = 2048;
    nix.settings.min-free = 0;
  };
in
pkgs.testers.runNixOSTest {
  name = "torq-adopt";

  nodes.gitserver = { pkgs, ... }: {
    services.openssh.enable = true;
    users.users.git = { isNormalUser = true; shell = pkgs.bash; };
    environment.systemPackages = [ pkgs.git ];
  };
  nodes.first = image;
  nodes.second = image;

  testScript = ''
    import json, shlex
    start_all()
    gitserver.wait_for_unit("sshd.service")
    gitserver.succeed("su - git -c 'git init -q --bare config.git && mkdir -p ~/.ssh && chmod 700 ~/.ssh'")
    repo = "/var/lib/torq/config"
    url = "ssh://git@gitserver/home/git/config.git"

    def on_console(m, text, timeout=120):
        try:
            m.wait_until_succeeds(f"grep -aqF {shlex.quote(text)} /dev/vcs1", timeout=timeout)
        except Exception:
            print(m.execute("tr -s ' ' < /dev/vcs1; echo; journalctl -b --no-pager -u torq-setup -u torq-setup-broker | tail -40")[1])
            raise

    def answer(m, text):
        m.send_chars(text + "\n")

    with subtest("first: a new machine, set up on its console; its repository and ledger on the server"):
        first.wait_for_unit("multi-user.target")
        on_console(first, "Start [1]:")
        answer(first, "")
        on_console(first, "Machine name [torq]:")
        answer(first, "first")
        on_console(first, "Time zone [UTC]:")
        answer(first, "")
        on_console(first, "Your user name:")
        answer(first, "alice")
        on_console(first, "Password:")
        answer(first, "Torq-7731-secret")
        on_console(first, "Password again:")
        answer(first, "Torq-7731-secret")
        on_console(first, "Choose [1]:")
        answer(first, "")
        on_console(first, "logging in: not from other machines")
        answer(first, "")
        first.wait_until_succeeds("test -s /var/lib/torq/state/ledger.jsonl", timeout=300)
        first.wait_for_unit("getty@tty1.service")
        # As its broker pushes them: main, and the ledger on torq-ledger.
        first.succeed("ssh-keygen -q -t ed25519 -N ''' -f /root/key")
        key = first.succeed("cat /root/key.pub").strip()
        gitserver.succeed(f"echo {shlex.quote(key)} > /home/git/.ssh/authorized_keys && chown git /home/git/.ssh/authorized_keys && chmod 600 /home/git/.ssh/authorized_keys")
        ssh = "ssh -F /dev/null -i /root/key -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/root/kh"
        first.succeed(
            f"cd {repo} && export GIT_SSH_COMMAND={shlex.quote(ssh)} && "
            "blob=$(git hash-object -w /var/lib/torq/state/ledger.jsonl) && "
            "tree=$(printf '100644 blob %s\\tledger.jsonl\\n' $blob | git mktree) && "
            "c=$(git -c user.name=t -c user.email=t@t commit-tree $tree -m ledger) && "
            f"git push -q {url} HEAD:refs/heads/main $c:refs/heads/torq-ledger"
        )
        entries = first.succeed("cat /var/lib/torq/state/ledger.jsonl")
        head = first.succeed(f"git -C {repo} rev-parse HEAD").strip()

    with subtest("second: from a repository; only SSH; nothing until its key is on the repository"):
        second.wait_for_unit("multi-user.target")
        on_console(second, "Start [1]:")
        answer(second, "2")
        on_console(second, "The repository's SSH URL")
        answer(second, "ext::sh -c touch% /tmp/pwned")
        on_console(second, "must be an SSH URL")
        second.fail("test -e /tmp/pwned")
        answer(second, url)
        on_console(second, "Press Enter when it is added")
        answer(second, "")
        on_console(second, "Try again? (yes / no)")
        second.fail(f"test -e {repo}/.git")
        pub = second.succeed("cat /var/lib/torq/state/remote/key.pub").strip()
        gitserver.succeed(f"echo {shlex.quote(pub)} >> /home/git/.ssh/authorized_keys")
        answer(second, "")
        on_console(second, "Press Enter when it is added")

    with subtest("second: what it found, read before anything changes; then it is that machine"):
        answer(second, "")
        on_console(second, "machine first, owner alice")
        on_console(second, "the remote's host key SHA256:")
        on_console(second, "will run on this machine as root")
        on_console(second, "Its SSH setting: not on")
        on_console(second, "own hardware file and software")
        second.fail("test -s /var/lib/torq/state/ledger.jsonl")
        on_console(second, "alice's password (on this machine):")
        answer(second, "Second-7731-secret")
        on_console(second, "Password again:")
        answer(second, "Second-7731-secret")
        on_console(second, "Become this machine?")
        answer(second, "yes")
        # The adoption's own entry, not just a ledger: the switch comes before it, and the console with it.
        second.wait_until_succeeds("tail -n 1 /var/lib/torq/state/ledger.jsonl | grep -q '\"kind\":\"adopted\"'", timeout=300)
        second.wait_for_unit("getty@tty1.service")
        out = second.succeed("cat /var/lib/torq/state/ledger.jsonl")
        assert out.startswith(entries), "the old machine's history, whole, is this one's"
        last = json.loads(out.splitlines()[-1])
        assert last["kind"] == "adopted" and last["data"]["url"] == url and last["data"]["from"] == head, last
        second.succeed("torq-broker verify && torq-broker status")
        # Its history goes on from the old machine's commit; the configuration is the old machine's.
        second.succeed(f"git -C {repo} merge-base --is-ancestor {head} HEAD")
        assert second.succeed(f"git -C {repo} rev-parse HEAD").strip() == last["data"]["commit"]
        second.succeed(f"jq -e '.user == \"alice\"' {repo}/nixos/torq.json")
        second.succeed("test \"$(cat /proc/sys/kernel/hostname)\" = first")
        # Its own password: outside the repo, and not the old machine's.
        second.succeed("test \"$(stat -c %a:%U /var/lib/torq/secrets/passwords/alice)\" = 600:root")
        assert second.succeed("cat /var/lib/torq/secrets/passwords/alice") != first.succeed("cat /var/lib/torq/secrets/passwords/alice")
        second.fail("grep -rlaF Second-7731-secret /var/lib/torq/config")
  '';
}
