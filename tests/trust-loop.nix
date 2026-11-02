# NixOS VM test: the M1 trust loop inside a real NixOS system.
# Rebuilds are stubbed (`true`) — this test proves broker/ledger/git behaviour, not nixos-rebuild.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-trust-loop";

  nodes.machine = { options, pkgs, ... }: {
    # A terminal to drive torqcli's line editor in.
    environment.systemPackages = [ pkgs.tmux ];
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    # NixOS's test instrumentation gives root an empty password, for logging in by hand; the driver
    # needs none (its own root shell), and TorqOS asserts root has none (modules/accounts.nix).
    users.users.root.hashedPasswordFile = pkgs.lib.mkForce null;
    # Rebuilds are stubbed through the broker's fixed settings (a root broker refuses a
    # TORQ_REBUILD_CMD from its caller). What a real machine would run is checked below.
    torq.broker.rebuildCommand = "true";
    environment.etc."torq-test/default-rebuild".text = options.torq.broker.rebuildCommand.default;
    environment.etc."torq-test/default-build".text = options.torq.broker.buildCommand.default;
    # commit confirmed: the "build" is a prebuilt specialisation (the VM cannot build systems), and
    # the boot default is the system profile only (the test VM has no boot loader). The activations —
    # into that system and back — the rollback timer, the old broker it runs, and the boot-time check
    # are real.
    specialisation.confirmtarget.configuration.environment.etc."torq-test/new-system".text = "yes";
    torq.broker.buildCommand = "readlink -f /run/booted-system/specialisation/confirmtarget";
    torq.broker.bootCommand = "nix-env -p {profile} --set {system}";
    specialisation.noguard.configuration.torq.broker.guardEtcNixos = false;
    # A switch to a configuration with another name: the running name follows (modules/host.nix).
    specialisation.renamed.configuration.networking.hostName = pkgs.lib.mkForce "renamed";
    # The test VM's disk is tiny: min-free would garbage-collect mid-test, deleting the unrooted
    # nixpkgs source the stray-rebuild check uses.
    nix.settings.min-free = 0;

    environment.etc."torq-test/desktop.nix".text = ''
      { nightLight = false; }
    '';
    environment.etc."torq-test/night-light.diff".text = ''
      --- a/desktop.nix
      +++ b/desktop.nix
      @@ -1 +1 @@
      -{ nightLight = false; }
      +{ nightLight = true; }
    '';
    environment.etc."torq-test/host.diff".text = ''
      --- /dev/null
      +++ b/system/host.json
      @@ -0,0 +1 @@
      +{ "host-name": "machine" }
    '';
    environment.etc."torq-test/policy.diff".text = ''
      --- /dev/null
      +++ b/policy/tiers.toml
      @@ -0,0 +1 @@
      +agent_can_do_anything = true
    '';
  };

  testScript = ''
    import json
    machine.wait_for_unit("multi-user.target")
    repo = "/var/lib/torq/config"
    # The boot-time drift check ran (a unit that never ran also reports Result=success, so check
    # that it started and what it said), and passed on a machine with nothing ledgered yet.
    machine.succeed("test -n \"$(systemctl show -p ExecMainStartTimestamp --value torq-status)\"")
    machine.succeed("journalctl -b -u torq-status | grep -q 'OK as far as recorded'")
    machine.succeed("test ! -e /run/torq/drift")
    # The broker builds the fixed configuration `torq`, not one named after the host (this host is
    # "machine"): changing the host name must never leave rebuilds looking for a missing config.
    machine.succeed("grep -qx 'nixos-rebuild switch --no-update-lock-file --flake {flake}#torq' /etc/torq-test/default-rebuild")
    # The build of a commit confirmed builds the commit too.
    machine.succeed("grep -q -- '--print-out-paths {flake}#nixosConfigurations.torq' /etc/torq-test/default-build")
    machine.succeed("jq -e '.rebuild == \"true\" and .repo == \"/var/lib/torq/config\"' /etc/torq/broker.json")
    # Undo's tools come from fixed store paths, not only from the (changeable) running system.
    machine.succeed("jq -r .path /etc/torq/broker.json | tr : '\\n' | head -1 | grep -q '^/nix/store/.*nixos-rebuild'")

    machine.succeed(f"mkdir -p {repo} && cp /etc/torq-test/desktop.nix {repo}/ && touch {repo}/flake.nix")
    machine.succeed("torq-broker init")

    with subtest("init unit never re-runs on an initialised repo"):
        # Re-running would sweep out-of-band edits into a 'baseline' commit, and during a broker
        # apply it fails the switch on the broker lock.
        machine.succeed(f"echo tampered > {repo}/stray.nix")
        machine.succeed("systemctl start torq-broker-init")
        machine.succeed("systemctl show -p ConditionResult torq-broker-init | grep -qx ConditionResult=no")
        machine.succeed(f"git -C {repo} status --porcelain | grep -q stray.nix")
        machine.succeed(f"rm {repo}/stray.nix")

    with subtest("agent proposes, user approves, change lands"):
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        out = machine.succeed(f"torq-broker propose --actor agent --base {head} "
                              "--diff /etc/torq-test/night-light.diff --reason 'Night light'")
        assert "proposal #2" in out, out
        machine.succeed("torq-broker apply 2 --approved-by tester")
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        machine.succeed(f"git -C {repo} log -1 --format=%B | grep -q 'Ledger-Entry: 3'")

    with subtest("one-command undo"):
        machine.succeed("torq-broker revert 3 --approved-by tester")
        machine.succeed(f"grep -q 'nightLight = false' {repo}/desktop.nix")

    with subtest("a root broker takes its settings only from /etc/torq/broker.json"):
        # A caller's override is refused, loudly, by variable or by flag.
        for override in ["TORQ_REBUILD_CMD='touch /tmp/pwned' torq-broker",
                         "TORQ_STATE_DIR=/tmp/fresh torq-broker",
                         "TORQ_CONFIG_REPO=/tmp/other torq-broker",
                         "torq-broker --state-dir /tmp/fresh"]:
            out = machine.fail(f"{override} verify 2>&1")
            assert "takes its settings only from /etc/torq/broker.json" in out, out
        machine.succeed("test ! -e /tmp/pwned -a ! -e /tmp/fresh")
        # Without the file, a root broker does not fall back to its caller's settings: it stops.
        machine.succeed("mv /etc/torq/broker.json /tmp/broker.json")
        out = machine.fail("TORQ_STATE_DIR=/tmp/fresh torq-broker verify 2>&1")
        assert "broker.json is missing" in out, out
        machine.succeed("mv /tmp/broker.json /etc/torq/broker.json")
        # Nothing of the caller's environment reaches git: a GIT_DIR elsewhere is ignored.
        machine.succeed("GIT_DIR=/nonexistent torq-broker status")

    with subtest("agent cannot touch policy"):
        # With its base, so it is refused for what it touches, not for leaving the base out.
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        out = machine.fail(f"torq-broker propose --actor agent --base {head} "
                           "--diff /etc/torq-test/policy.diff --reason 'loosen permissions' 2>&1")
        assert "forbidden" in out, out

    with subtest("ledger is tamper-evident"):
        machine.succeed("torq-broker verify")
        machine.succeed("cp /var/lib/torq/state/ledger.jsonl /tmp/ledger.good")
        machine.succeed("sed -i 's/Night light/Nothing here/' /var/lib/torq/state/ledger.jsonl")
        machine.fail("torq-broker verify")
        machine.succeed("systemctl start torq-status")  # never fails (it runs during switches)...
        machine.succeed("grep -q 'ledger' /run/torq/drift")  # ...but reports it
        machine.succeed("cp /tmp/ledger.good /var/lib/torq/state/ledger.jsonl")  # for what follows
        machine.succeed("systemctl start torq-status && test ! -e /run/torq/drift")

    with subtest("torqcli reads the configuration as a user, by git plumbing only"):
        out = machine.succeed("torq-broker --json propose --diff /etc/torq-test/host.diff --reason host")
        machine.succeed(f"torq-broker apply {json.loads(out)['seq']} --approved-by tester")
        # The repo belongs to root; an ordinary user reads it (safe.directory), and nothing in their
        # environment points git elsewhere or configures it.
        hostile = "GIT_DIR=/nonexistent GIT_CONFIG_PARAMETERS=bogus GIT_INDEX_FILE=/nonexistent"
        out = machine.succeed(f"su - dev -c '{hostile} torqcli -c \"show configuration\"'")
        assert out.startswith("networking {\n    hostName machine;\n}\n"), out
        # The whole device: what is set outside the tree is named, as a comment a saved file keeps;
        # flake.nix is TorqOS's own, not set outside it.
        assert "## also set outside this configuration, in Nix: desktop.nix\n" in out, out
        out = machine.succeed("su - dev -c 'torqcli -c \"configure; set system time-zone UTC; show | compare\"'")
        assert "[edit time]\n+   timeZone UTC;" in out, out
        # The people here, as the running machine has them: the owner alone (no system account), with
        # TorqOS's groups for them, their own first; and where groups are set.
        out = machine.succeed("su - dev -c 'torqcli -c \"show users\"'")
        lines = out.splitlines()
        assert lines[0].split() == ["User", "Logged", "in", "Groups"] and lines[1].startswith("dev "), out
        assert lines[1].split()[-4:] == ["users", "wheel", "audio", "video"], out
        assert lines[2].startswith("A person's groups: `set users users <name> extraGroups <group>`"), out
        # save / load: as the user, private; a saved configuration loaded back is the same candidate.
        out = machine.succeed("su - dev -c 'torqcli -c \"configure; set system time-zone UTC; save /home/dev/saved.conf; rollback 0; load override /home/dev/saved.conf; show | compare\"'")
        assert "Wrote " in out and "[edit time]\n+   timeZone UTC;" in out, out
        machine.succeed("su - dev -c 'test \"$(stat -c %a /home/dev/saved.conf)\" = 600'")
        # -c uses a private candidate; the interactive shell keeps one, private, between sessions.
        machine.succeed("su - dev -c 'test ! -e ~/.local/state/torq/candidate.json'")
        machine.succeed("su - dev -c 'printf \"configure\\nset users dev packages jq\\nexit\\nexit\\n\" | torqcli'")
        machine.succeed("su - dev -c 'test \"$(stat -c %a ~/.local/state/torq/candidate.json)\" = 600'")
        # The ledger is root's (its state directory is 0700): the numbers come through the broker, as a
        # commit does. Without it there are no numbers, rather than git's (which a rollback can differ from).
        out = machine.fail("su - dev -c 'torqcli -c \"show system commit\"' 2>&1")
        assert "could not read the ledger" in out, out
        out = machine.succeed("torqcli -c 'show system commit'")
        # (A heading, then the newest configuration: 0.)
        assert out.splitlines()[0].startswith("Rollback") and " host" in out.splitlines()[1] and "tester" in out.splitlines()[1], out
        out = machine.fail("su - dev -c 'torqcli -c \"configure; set system host-name my_laptop\"' 2>&1")
        assert "is not a valid system host-name" in out, out
        # Piped input is a script: it stops at its first error (exit 1) and runs nothing after it.
        out = machine.fail("su - dev -c 'printf \"configure\\nset system host-name my_laptop\\nshow | compare\\n\" | torqcli' 2>&1")
        assert "is not a valid system host-name" in out and "[edit" not in out.split("my_laptop")[-1], out
        # One syntax (ADR-021): the other one's words are unknown commands, not another meaning.
        out = machine.fail("su - dev -c 'torqcli -c \"configure; no system time-zone\"' 2>&1")
        assert "'no' is not a word here" in out, out

    with subtest("torqcli filters: the real tools, in a sandbox that reaches nothing"):
        import shlex
        def dev_sh(cmd):
            return f"su - dev -c {shlex.quote(cmd)}"
        # The tools are the system's own: a grep first on PATH is not the one that runs.
        machine.succeed("mkdir -p /tmp/evil && printf '#!/bin/sh\\necho evil\\n' > /tmp/evil/grep && chmod 755 /tmp/evil /tmp/evil/grep")
        out = machine.succeed(dev_sh("PATH=/tmp/evil:$PATH torqcli -c 'show configuration | grep -c hostName'"))
        assert out == "1\n", out
        out = machine.succeed(dev_sh("torqcli 'show configuration | grep -o \"hostName [a-z]*\"'"))
        assert out == "hostName machine\n", out
        out = machine.succeed(dev_sh("torqcli show configuration '|' head -n 1 '|' wc -l"))
        assert out == "1\n", out
        # Their own options cannot read, write or run anything outside the output they filter.
        machine.succeed(dev_sh("echo hidden > ~/secret"))
        out = machine.fail(dev_sh("torqcli -c 'show configuration | head -n 1 /home/dev/secret' 2>&1"))
        assert "No such file" in out and "hidden" not in out, out
        machine.fail(dev_sh("torqcli -c 'show configuration | sort -o /home/dev/written' 2>&1"))
        machine.succeed("test ! -e /home/dev/written")
        # What a filter prints reaches the terminal cleaned, like everything else.
        out = machine.succeed(dev_sh("torqcli -c \"show configuration | tr s '\\\\033'\""))
        assert "\x1b" not in out and "ho\\u{1b}tName machine;" in out, repr(out)
        # What is behind a view, only when asked.
        out = machine.succeed(dev_sh("torqcli 'show configuration | display detail'"))
        assert "## commit " in out and "## system/host.json: networking hostName\n" in out, out
        assert "## commit " not in machine.succeed(dev_sh("torqcli show configuration")), "no internals unless asked"
        # Every NixOS setting of the system, TorqOS's too, generated with it for torqcli.
        schema = json.loads(machine.succeed("cat /etc/torq/nixos-schema.json"))
        # (Settings have a type; a section that takes any key of its own says what each key takes.)
        locs = {".".join(s["loc"]): s["type"] for s in schema["settings"] if "type" in s}
        free = {".".join(s["loc"]) for s in schema["settings"] if "free" in s}
        assert "services.openssh.settings" in free, sorted(free)[:5]
        assert locs["virtualisation.docker.enable"] == {"name": "bool"}, locs.get("virtualisation.docker.enable")
        assert "torq.dev.enable" in locs and "services.httpd.virtualHosts.<name>.forceSSL" in locs
        assert len(locs) > 10000, len(locs)
        # The commands before torqcli say what replaced them.
        out = machine.fail(dev_sh("torq pkg add htop 2>&1"))
        assert "is gone" in out and "set users <you> packages <name>" in out, out
        # The ledger as a person reads it, through the broker (root here, as sudo would be).
        out = machine.succeed("torqcli show system ledger")
        assert "host" in out, out
        # Reverting a system change needs a person at a terminal: a script cannot.
        applied = json.loads(machine.succeed("torq-broker --json log --brief | grep '\"kind\":\"applied\"' | tail -n 1"))["seq"]
        before = machine.succeed("git -C /var/lib/torq/config rev-parse HEAD")
        out = machine.fail(f"torqcli request system revert {applied} < /dev/null 2>&1")
        assert "needs a person to approve it at a terminal" in out, out
        assert machine.succeed("git -C /var/lib/torq/config rev-parse HEAD") == before, "nothing changed"
        # One command, no -c: a configuration change is refused, and says how.
        out = machine.fail(dev_sh("torqcli configure 2>&1"))
        assert "torqcli -c" in out, out

    with subtest("TorqOS's shell: zsh with oh-my-zsh and its prompt, for every user, at once"):
        import shlex
        def dev(cmd):
            return machine.succeed(f"su - dev -c {shlex.quote(cmd)}")
        machine.succeed("getent passwd dev | cut -d: -f7 | grep -q '/zsh$'")
        dev("rm -f ~/.zshrc ~/.local/state/torq/candidate.json")
        dev("tmux new-session -d -s z -x 120 -y 30 zsh")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote('tmux capture-pane -p -t z | grep -q ❯')}", timeout=60)
        out = dev("tmux capture-pane -p -t z")
        # No set-up questions for a new user; who and where, and nothing of TorqOS's to say.
        assert "dev@machine" in out and "configuration function" not in out and "candidate" not in out, out
        # A candidate waiting shows in the next prompt.
        dev("mkdir -p ~/.local/state/torq && echo '{}' > ~/.local/state/torq/candidate.json")
        dev("tmux send-keys -t z Enter")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote('tmux capture-pane -p -t z | grep -q \"● candidate\"')}", timeout=30)
        dev("rm -f ~/.local/state/torq/candidate.json; tmux kill-session -t z")

    with subtest("torqcli on a terminal: ? lists what may come next, Tab completes"):
        import shlex
        def as_dev(cmd, ok=True):
            run = machine.succeed if ok else machine.fail
            return run(f"su - dev -c {shlex.quote(cmd)}")
        screen = "tmux capture-pane -p -t t"
        def on_screen(test):
            # Waits for the screen to pass `test`; if it never does, the log shows the screen.
            try:
                machine.wait_until_succeeds(f"su - dev -c {shlex.quote(screen + test)}", timeout=30)
            except Exception:
                print(as_dev(screen))
                raise
        as_dev("tmux new-session -d -s t -x 160 -y 50 torqcli")
        on_screen(' | grep -q dev@machine')
        as_dev("tmux send-keys -t t configure Enter")
        on_screen(' | grep -q dev@machine#')
        as_dev("tmux send-keys -t t 'commit ' '?'")
        on_screen(' | grep -q Possible.completions')
        out = as_dev(screen)
        assert "<[Enter]>" in out and "Execute this command" in out and "confirmed" in out, out
        # The line is still there after the list, ready to finish.
        assert out.rstrip().splitlines()[-1].endswith("commit"), out
        as_dev("tmux send-keys -t t C-u 'set time ti' Tab")
        # (capture-pane drops trailing spaces, so the space Tab adds is not seen here.)
        on_screen(' | grep -q \"set time timeZone\"')
        # Where configuration mode is shows above the prompt.
        as_dev("tmux send-keys -t t C-u 'edit system' Enter")
        on_screen(' | grep -qF \"[edit system]\"')
        as_dev("tmux send-keys -t t top Enter")
        on_screen(' | grep -v \"^$\" | tail -n 2 | head -n 1 | grep -qxF \"[edit]\"')
        # A paste runs every line as it arrives (no Enter after it), each error naming its line — and a
        # commit after a failed line asks first, a question the paste cannot answer.
        as_dev("tmux send-keys -t t C-u")
        as_dev("printf 'set system host-name my_laptop\\nset users dev packages vim\\ncommit\\nshow | compare\\n' | tmux load-buffer - && tmux paste-buffer -p -t t")
        on_screen(' | grep -q \"1 line failed since you last looked at\"')
        out = as_dev(screen)
        assert "error: set system host-name my_laptop:" in out, out
        as_dev("tmux send-keys -t t n Enter")
        on_screen(' | grep -q \"error: commit: not committed\"')
        # A person reads the answer before typing on: a line the prompt did not wait for is taken as
        # more of the paste the question ended (WAITING, cli/src/shell.rs), and not run.
        machine.sleep(1)
        as_dev("tmux send-keys -t t 'show | compare' Enter")
        on_screen(' | grep -qE \"^\\+   packages \\[.* vim \\];\"')
        # Without bracketed paste (a serial console, screen): the same, and no line is lost.
        as_dev("printf 'set users dev packages htop\\nset users dev packages jq\\nshow | compare\\n' | tmux load-buffer - && tmux paste-buffer -t t")
        on_screen(' | grep -qE \"^\\+   packages \\[.*(htop.*vim|vim.*htop).*\\];\"')
        # `load merge terminal`: a configuration pasted in brace format (a comment in it is left out),
        # ended by Ctrl-D, as on a network device.
        as_dev("tmux send-keys -t t 'load merge terminal' Enter")
        on_screen(' | grep -qF \"[Type ^D at a new line to end input]\"')
        as_dev("printf 'time {\\n    /* where it stands */\\n    timeZone Europe/Berlin;\\n}\\n' | tmux load-buffer - && tmux paste-buffer -t t")
        as_dev("tmux send-keys -t t C-d")
        on_screen(' | grep -q \"load complete\"')
        machine.sleep(1)  # (as after the question above: read, then type)
        as_dev("tmux send-keys -t t 'show | compare' Enter")
        on_screen(' | grep -qF \"+   timeZone Europe/Berlin;\"')
        # Leave.
        last_line = screen + " | grep -v '^$' | tail -n 1"
        as_dev("tmux send-keys -t t exit Enter")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote(last_line + ' | grep -q \"dev@machine>\"')}", timeout=30)
        as_dev("tmux send-keys -t t exit Enter")
        machine.wait_until_fails(f"su - dev -c {shlex.quote('tmux has-session -t t')}", timeout=30)
        # A config file: `file edit` opens $EDITOR (here a script that writes a module) at the
        # terminal; it waits in the candidate, attached, and shows as its raw diff.
        machine.succeed("printf '{ ... }:\\n{\\n  environment.etc.greeting.text = \"hi\";\\n}\\n' > /home/dev/greeting.nix")
        machine.succeed("printf '#!/bin/sh\\ncp /home/dev/greeting.nix \"$1\"\\n' > /home/dev/ed.sh && chmod 755 /home/dev/ed.sh /home/dev/greeting.nix")
        as_dev("tmux new-session -d -s f -x 160 -y 50 'EDITOR=/home/dev/ed.sh torqcli'")
        as_dev("tmux send-keys -t f configure Enter")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote('tmux capture-pane -p -t f | grep -q \"configuration mode\"')}", timeout=30)
        as_dev("tmux send-keys -t f 'file edit custom/greeting.nix' Enter")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote('tmux capture-pane -p -t f | grep -q \"custom/greeting.nix: changed in the candidate\"')}", timeout=30)
        as_dev("printf 'set environment config custom/greeting.nix\\nshow | compare\\n' | tmux load-buffer - && tmux paste-buffer -p -t f")
        as_dev("tmux send-keys -t f Enter")
        machine.wait_until_succeeds(f"su - dev -c {shlex.quote('tmux capture-pane -p -t f -S -100 | grep -qF \"+  environment.etc.greeting.text\"')}", timeout=30)
        out = as_dev("tmux capture-pane -p -t f -S -100")
        assert "+   config custom/greeting.nix;" in out and "+++ b/custom/greeting.nix" in out, out
        as_dev("tmux send-keys -t f 'rollback 0' Enter")
        as_dev("tmux kill-session -t f")
        machine.succeed("test ! -e /var/lib/torq/config/custom")  # nothing committed

    with subtest("a stray nixos-rebuild cannot switch away from the ledgered config"):
        # The owner's files are moved aside, never overwritten or opened: a config that merely
        # mentions the option, and a symlink to a FIFO (reading it would hang activation at boot).
        machine.succeed("rm -f /etc/nixos/configuration.nix /etc/nixos/flake.nix /etc/nixos/*.pre-torq*")
        machine.succeed("echo '{ torq.broker.guardEtcNixos = false; }' > /etc/nixos/configuration.nix")
        machine.succeed("mkfifo /tmp/fifo && ln -s /tmp/fifo /etc/nixos/flake.nix")
        machine.succeed("timeout 120 /run/current-system/activate")
        machine.succeed("grep -qx '{ torq.broker.guardEtcNixos = false; }' /etc/nixos/configuration.nix.pre-torq")
        machine.succeed("test \"$(readlink /etc/nixos/flake.nix.pre-torq)\" = /tmp/fifo")
        # The stubs are real files; re-activating keeps them and makes no more copies.
        machine.succeed("timeout 120 /run/current-system/activate")
        machine.succeed("test -f /etc/nixos/flake.nix -a ! -L /etc/nixos/flake.nix")
        machine.succeed("test -f /etc/nixos/configuration.nix -a ! -L /etc/nixos/configuration.nix")
        machine.fail("ls /etc/nixos/*.pre-torq.*")
        # Building /etc/nixos refuses, every common way, with the message pointing at the broker.
        # (With flake.nix present nixos-rebuild uses it even when given -I, so --no-flake is what
        # reaches the configuration.nix stub.)
        for cmd in [
            "nixos-rebuild dry-build -I nixos-config=/etc/nixos/configuration.nix",  # what went wrong
            # The VM has no channels; the machine this happened on did, so supply nixpkgs.
            "nixos-rebuild dry-build --no-flake -I nixpkgs=${pkgs.path} -I nixos-config=/etc/nixos/configuration.nix",
            "nixos-rebuild dry-build",
            "nix --extra-experimental-features 'nix-command flakes' eval "
            "path:/etc/nixos#nixosConfigurations.machine.config.system.build.toplevel",
        ]:
            out = machine.fail(cmd + " 2>&1")
            assert "managed by TorqOS" in out, f"{cmd}: {out}"
        # Turning the guard off removes the stubs; the owner's files stay where they were put.
        machine.succeed("/run/current-system/specialisation/noguard/bin/switch-to-configuration test")
        machine.succeed("test ! -e /etc/nixos/flake.nix -a ! -e /etc/nixos/configuration.nix")
        machine.succeed("test -e /etc/nixos/configuration.nix.pre-torq -a -L /etc/nixos/flake.nix.pre-torq")

    with subtest("a machine switched outside the broker is reported"):
        # The guard subtest just switched to another configuration (the specialisation) without the
        # broker: the running system is no longer the last ledgered build. (That switch also ran the
        # check mid-switch, and succeeded: a monitor must not be able to fail a switch.)
        machine.succeed("systemctl start torq-status")
        machine.succeed("grep -q 'running system' /run/torq/drift")
        machine.succeed("journalctl -b -u torq-status -p warning | grep -q 'PROBLEM: running system'")
        out = machine.succeed("bash -ic true 2>&1")
        assert "not as its ledger says" in out, out
        # torqcli reports it in its own words, without raw colour codes, and will not repair it
        # without a person's yes (no terminal here: a script cannot switch the system).
        out = machine.succeed("torqcli -c 'show system alarms'")
        assert "not as its history says" in out and "request system repair" in out, out
        assert "\\u{1b}" not in out and "\x1b" not in out, out
        out = machine.fail("torqcli -c 'request system repair' 2>&1")
        assert "needs a person to approve it" in out, out
        machine.succeed("systemctl start torq-status && grep -q 'running system' /run/torq/drift")
        # Back on the ledgered system, all is well again.
        machine.succeed("/run/booted-system/bin/switch-to-configuration test")
        machine.succeed("systemctl start torq-status")
        machine.succeed("test ! -e /run/torq/drift")
        out = machine.succeed("bash -ic true 2>&1")
        assert "not as its ledger says" not in out, out

    def broker(args):
        return json.loads(machine.succeed(f"torq-broker --json {args}"))
    def last_entry():
        return json.loads(machine.succeed("tail -n1 /var/lib/torq/state/ledger.jsonl"))
    def propose_night_light():
        return broker("propose --diff /etc/torq-test/night-light.diff --reason 'Night light'")["seq"]

    booted = machine.succeed("readlink -f /run/booted-system").strip()
    new_system = machine.succeed("readlink -f /run/booted-system/specialisation/confirmtarget").strip()
    # A real machine has a system profile (its boot default); the test VM boots without one. A commit
    # confirmed needs to know what the next boot starts, so give it the one it would have.
    machine.succeed(f"nix-env -p /nix/var/nix/profiles/system --set {booted}")
    def running():
        return machine.succeed("readlink -f /run/current-system").strip()

    with subtest("commit confirmed: rolled back by its timer unless confirmed, retrying while busy"):
        assert running() == booted
        a = broker(f"apply {propose_night_light()} --approved-by tester --confirm 1")
        # The new system really runs; the boot default is untouched.
        assert running() == new_system and a["data"]["system"] == new_system, a
        machine.succeed("test -e /etc/torq-test/new-system")
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        # A real wall-clock timer, running the broker that made the change by its store path.
        unit = f"torq-confirm-{a['seq']}"
        machine.succeed(f"systemctl is-active {unit}.timer")
        machine.succeed(f"systemctl show -p TimersCalendar {unit}.timer | grep -q 'UTC'")
        machine.succeed(f"systemctl show -p ExecStart {unit}.service "
                        f"| grep -Eq 'argv\\[\\]=/nix/store/[^ ]+/bin/torq-broker expire-confirm {a['seq']} '")
        out = machine.succeed("torq-broker status")
        assert "rolls back at" in out, out
        out = machine.fail(f"torq-broker revert {a['seq']} --approved-by tester 2>&1")
        assert "waiting for confirmation" in out, out
        # Another operation holds the broker lock past the deadline: the timer's run is refused as
        # busy, and retried until it gets through.
        machine.succeed("systemd-run --unit=hold-lock flock /var/lib/torq/state/broker.lock "
                        "/run/current-system/sw/bin/sleep 100")
        machine.succeed("systemctl is-active hold-lock")
        machine.wait_until_succeeds(f"journalctl -u {unit}.service | grep -q 'another broker operation is running'", timeout=150)
        assert running() == new_system, "not rolled back while busy"
        # The ledger entry is the last step (the config is reset first, then the system activated).
        machine.wait_until_succeeds("tail -n1 /var/lib/torq/state/ledger.jsonl | grep -q rolled_back", timeout=180)
        machine.succeed(f"grep -q 'nightLight = false' {repo}/desktop.nix")
        e = last_entry()
        assert e["kind"] == "rolled_back" and e["data"]["unconfirmed"] and not e["data"]["by_reboot"], e
        assert e["data"]["restored"] and e["data"]["applied"] == a["seq"], e
        assert running() == booted, "the old system runs again"
        machine.succeed("test ! -e /etc/torq-test/new-system")
        machine.succeed("torq-broker status")

    with subtest("commit confirmed: a restart before confirming rolls back"):
        a = broker(f"apply {propose_night_light()} --approved-by tester --confirm 30")
        assert running() == new_system
        machine.shutdown()
        machine.start()
        machine.wait_for_unit("multi-user.target")
        assert running() == booted, "the boot default was never changed"
        machine.wait_until_succeeds("tail -n1 /var/lib/torq/state/ledger.jsonl | grep -q rolled_back", timeout=120)
        e = last_entry()
        assert e["data"]["by_reboot"] and e["data"]["restored"] and e["data"]["applied"] == a["seq"], e
        machine.succeed(f"grep -q 'nightLight = false' {repo}/desktop.nix")
        machine.succeed("torq-broker status")
        machine.succeed("torq-broker verify")

    with subtest("commit confirmed: confirmed in time, it stays"):
        a = broker(f"apply {propose_night_light()} --approved-by tester --confirm 1")
        c = broker("confirm --approved-by tester")
        assert c["kind"] == "confirmed" and c["data"]["applied"] == a["seq"], c
        # The timer still fires at the deadline, and finds nothing to roll back.
        machine.wait_until_succeeds(f"journalctl -u torq-confirm-{a['seq']}.service | grep -q 'nothing is waiting'", timeout=180)
        assert running() == new_system
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        assert last_entry()["kind"] == "confirmed"
        machine.succeed("torq-broker status")

    with subtest("a switch gives the machine the name its configuration says, at once"):
        assert machine.succeed("hostname").strip() == "machine"
        machine.succeed("/run/booted-system/specialisation/renamed/bin/switch-to-configuration test")
        assert machine.succeed("hostname").strip() == "renamed", machine.succeed("hostname")
        machine.succeed("/run/booted-system/bin/switch-to-configuration test")
        assert machine.succeed("hostname").strip() == "machine"
  '';
}
