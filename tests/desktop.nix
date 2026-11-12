# NixOS VM test: TorqOS Desktop (ADR-045) boots to COSMIC's greeter, and the owner's password logs in to
# a COSMIC session. On it: NetworkManager with iwd as its Wi-Fi backend (one Wi-Fi stack, ADR-046 2), no
# wpa_supplicant; Flatpak with Flathub for the person only, never the system's (ADR-037, amended).
# Smoothness is judged on real hardware, not here.
{ pkgs, self }:

let
  # The owner's password, `torq-desktop`, as the broker keeps it (modules/accounts.nix).
  hash = "$6$torqtestsalt$wXsKSX3z/kTvGvAzmdrks4iI95ZmWWtQgfYUMV/OEbQ7h7HZxSpMJYh0hvWPma/izeabeQYfkEhhQhWA52xbE1";
in
pkgs.testers.runNixOSTest {
  name = "torq-desktop";
  # The desktop sets nixpkgs.overlays (COSMIC from its own nixpkgs), so the nodes build their own package set.
  node.pkgsReadOnly = false;

  nodes.machine = { lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "alice";
    torq.desktop.enable = true;
    torq.desktop.dimInsteadOfSuspend = true;
    # As every TorqOS machine has it (lib.mkSystem); the desktop reads it to take COSMIC from its own nixpkgs.
    nixpkgs.hostPlatform = pkgs.stdenv.hostPlatform.system;
    # NixOS's test instrumentation gives root an empty password; TorqOS asserts root has none.
    users.users.root.hashedPasswordFile = lib.mkForce null;
    # COSMIC's session and its applications: NixOS's own COSMIC test gives it 4 GB.
    virtualisation.memorySize = 4096;
    # What a change does here does not matter: this proves who may approve it, and how.
    torq.broker.rebuildCommand = "true";
    environment.etc."torq-test/desktop.nix".text = "{ nightLight = false; }\n";
    environment.etc."torq-test/system.diff".text = ''
      --- a/desktop.nix
      +++ b/desktop.nix
      @@ -1 +1 @@
      -{ nightLight = false; }
      +{ nightLight = true; }
    '';
    environment.systemPackages = [ pkgs.pam ];
  };

  testScript = ''
    import json, shlex
    machine.wait_for_unit("multi-user.target")
    # The owner's password, set as setup sets it, then the accounts written from it.
    machine.succeed("install -d -m 700 /var/lib/torq/secrets/passwords"
                    " && echo '${hash}' > /var/lib/torq/secrets/passwords/alice"
                    " && /run/current-system/activate")

    with subtest("the greeter"):
        machine.wait_for_unit("graphical.target", timeout=180)
        machine.wait_until_succeeds("pgrep -u cosmic-greeter -f cosmic-greeter", timeout=60)
        # greetd draws the password field a little after the greeter starts.
        machine.sleep(10)
        machine.screenshot("greeter")

    with subtest("the owner logs in to COSMIC"):
        # Typed again if the greeter had not yet drawn its password field (on a slower host it may not have).
        for attempt in range(4):
            machine.send_chars("torq-desktop\n", delay=0.2)
            if machine.execute("timeout 60 sh -c 'until pgrep -u alice -fx cosmic-panel; do sleep 2; done'")[0] == 0:
                break
            machine.sleep(5)
        # COSMIC's programs run wrapped: their names are the wrapper's, their command lines their own.
        machine.wait_until_succeeds("pgrep -u alice -fx cosmic-comp", timeout=120)
        machine.wait_until_succeeds("pgrep -u alice -fx cosmic-panel", timeout=120)
        machine.sleep(10)
        machine.screenshot("session")

    with subtest("one Wi-Fi stack: NetworkManager on iwd"):
        machine.wait_for_unit("NetworkManager.service")
        machine.succeed("NetworkManager --print-config | grep -qx 'wifi.backend=iwd'")
        machine.fail("systemctl cat wpa_supplicant.service")

    with subtest("TorqOS Settings is in the menu"):
        machine.succeed("test -e /run/current-system/sw/share/applications/org.torqos.Settings.desktop")
        machine.succeed("test -x /run/current-system/sw/bin/torq-settings")
        # And TorqOS in the panel (ADR-049): an applet COSMIC offers in its panel settings.
        machine.succeed("grep -qx X-CosmicApplet=true /run/current-system/sw/share/applications/org.torqos.Applet.desktop")
        machine.succeed("test -x /run/current-system/sw/bin/torq-applet")
        # In the panel by default, running there: a new machine has it without anyone adding it.
        machine.wait_until_succeeds("pgrep -u alice -fx torq-applet", timeout=60)

    with subtest("COSMIC's own first-run wizard is not offered: TorqOS has its setup"):
        machine.fail("test -e /run/current-system/sw/bin/cosmic-initial-setup")

    with subtest("COSMIC starts its panel again by itself, with the applet in it: what a switch relies on"):
        panel = machine.succeed("pgrep -u alice -fx cosmic-panel").strip()
        machine.succeed("pkill -u alice -fx cosmic-panel")
        machine.wait_until_succeeds(f"pgrep -u alice -fx cosmic-panel | grep -vx {panel}", timeout=60)
        machine.wait_until_succeeds("pgrep -u alice -fx torq-applet", timeout=60)
        machine.succeed("pgrep -u alice -fx cosmic-comp")

    with subtest("memory run short ends a program, it does not freeze the desktop"):
        machine.succeed("swapon --show=NAME --noheadings | grep -q zram")

    with subtest("a system change from the desktop is approved in COSMIC's dialog, and only there"):
        repo = "/var/lib/torq/config"
        machine.succeed(f"mkdir -p {repo} && cp /etc/torq-test/desktop.nix {repo}/ && touch {repo}/flake.nix")
        machine.succeed("torq-broker init")
        sock = "${pkgs.socat}/bin/socat -t 150 - UNIX-CONNECT:/run/torq/broker.sock"
        def on_the_desktop(request):
            # A program of alice's desktop session, as an application is: in her user manager.
            line = json.dumps(request)
            shell = f"printf '%s\\n' {shlex.quote(line)} | {sock}"
            return f"systemd-run --user --machine=alice@ --pipe --wait --quiet /bin/sh -c {shlex.quote(shell)}"
        def frames(raw):
            code, out = None, ""
            for l in raw.splitlines():
                f = json.loads(l)
                out += f.get("out", "")
                if "exit" in f:
                    code = f["exit"]
            return code, json.loads(out) if out.strip() else None
        head = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        diff = machine.succeed("cat /etc/torq-test/system.diff")
        code, p = frames(machine.succeed(on_the_desktop({"args": ["--json", "propose", f"--base={head}", "--reason=night light"], "diff": diff})))
        assert code == 0, (code, p)
        seq = str(p["seq"])

        # A password sent straight to the broker, right or not, is refused from here.
        code, v = frames(machine.succeed(on_the_desktop({"args": ["--json", "apply", seq], "password": "torq-desktop"})))
        assert code == 1 and "dialog" in v["error"], (code, v)
        machine.fail(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")

        # Only people reach the password helper: not a service, and not the agent.
        machine.succeed("test \"$(stat -c %a:%G /run/polkit/agent-helper.socket)\" = 660:users")
        machine.fail("su -s /bin/sh torq-agent -c '${pkgs.socat}/bin/socat -u OPEN:/dev/null UNIX-CONNECT:/run/polkit/agent-helper.socket'")
        # A wrong password in the dialog counts toward the same lockout as one at a terminal.
        ask = on_the_desktop({"args": ["--json", "apply", seq]})
        def ask_in_the_dialog(out):
            # Asked until COSMIC's dialog has registered with polkit: on a slow, software-rendered desktop (.52)
            # it was still starting 25 s after login, and the broker said so ("no authentication agent is
            # running"), as it would to a person; torqcli asks again the same way. A dialog that never registers
            # (one left on a polkit that has gone) fails it, after about two minutes.
            for attempt in range(8):
                machine.succeed(f"(setsid {ask} > {out} 2>&1 < /dev/null &)")
                # A dialog is a pkcheck still waiting a second later: without one, pkcheck ends at once.
                asked, _ = machine.execute(f"timeout 30 sh -c 'until pgrep -x pkcheck >/dev/null && sleep 1 && pgrep -x pkcheck >/dev/null; do grep -q \"no authentication agent\" {out} && exit 1; sleep 0.2; done'")
                if asked == 0:
                    return
                print(machine.execute(f"cat {out}; pgrep -u alice -fa cosmic-osd")[1])
                assert "no authentication agent" in machine.succeed(f"cat {out}"), "no dialog, and not for want of one"
                machine.sleep(10)
            raise AssertionError("COSMIC's dialog never registered with polkit")
        ask_in_the_dialog("/tmp/refused.out")
        machine.sleep(8)
        machine.screenshot("approval")
        machine.send_chars("wrong-password\n", delay=0.1)
        machine.wait_until_succeeds("test $(faillock --dir /run/torq/faillock --user alice | wc -l) -gt 2", timeout=30)
        machine.fail(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")
        machine.succeed("pkill -x pkcheck || true")
        machine.sleep(3)

        # polkit replaced, as a switch that changes it does: the applet has COSMIC's agent register again. Even an
        # applet that itself starts after the replacement (the panel restarting it, a crash): it reads the processes'
        # start times, not a pid it remembered (the full check on .52, 2026-11-10, where it did not).
        # A fresh applet by restarting the panel: COSMIC 1.11's panel does not start again an applet that exits,
        # only its applets when it starts itself (the laptop's run of this test).
        applet = machine.succeed("pgrep -u alice -fx torq-applet").strip()
        osd = machine.succeed("pgrep -u alice -fx cosmic-osd").strip()
        machine.succeed("systemctl restart polkit")
        machine.succeed("pkill -u alice -fx cosmic-panel")
        machine.wait_until_succeeds(f"pgrep -u alice -fx torq-applet | grep -vx {applet}", timeout=120)
        machine.wait_until_succeeds(f"pgrep -u alice -fx cosmic-osd | grep -vx {osd}", timeout=120)
        machine.sleep(3)
        # A process another account names polkitd is not polkit: newer than the dialog, it must not have the
        # applet end it (the agent could otherwise end the person's dialog every few seconds).
        osd = machine.succeed("pgrep -u alice -fx cosmic-osd").strip()
        machine.succeed("cp ${pkgs.bash}/bin/bash /tmp/polkitd && chmod 755 /tmp/polkitd")
        machine.succeed("systemd-run --uid=nobody --unit=fake-polkitd /tmp/polkitd -c '${pkgs.coreutils}/bin/sleep 120; true'")
        machine.wait_until_succeeds("pgrep -u nobody -x polkitd", timeout=10)
        machine.sleep(12)
        machine.succeed(f"pgrep -u alice -fx cosmic-osd | grep -qx {osd}")
        machine.succeed("systemctl stop fake-polkitd")

        # Meanwhile the applet saved alice's desktop (it saves when the ledger changes, and every 15
        # minutes): the configuration has moved on from the change's base, by a commit to her own folder.
        # The regression of ADR-053: it was refused as stale, so no change waiting for a person could land.
        machine.wait_until_succeeds(f"git -C {repo} cat-file -e HEAD:users/alice/desktop.json", timeout=150)
        moved = machine.succeed(f"git -C {repo} rev-parse HEAD").strip()
        assert moved != head, moved

        # The right one, in the dialog: the change is made.
        ask_in_the_dialog("/tmp/approved.out")
        machine.sleep(8)
        machine.send_chars("torq-desktop\n", delay=0.1)
        try:
            machine.wait_until_succeeds("grep -q '\"exit\":0' /tmp/approved.out", timeout=60)
        except Exception:
            # What the broker answered, and what it and polkit logged: a failure here says why.
            print(machine.execute("cat /tmp/approved.out; journalctl -b --no-pager -u 'torq-broker@*' -u polkit -u 'polkit-agent-helper@*' | tail -40")[1])
            raise
        machine.succeed(f"git -C {repo} show HEAD:desktop.nix | grep -q 'nightLight = true'")
        # Re-checked on the moved configuration, and the history says so; the desktop save kept.
        applied = [json.loads(l) for l in machine.succeed("torq-broker --json log --brief").splitlines()]
        applied = [e for e in applied if e["kind"] == "applied" and e["data"].get("proposal") == int(seq)]
        assert len(applied) == 1 and applied[0]["data"].get("rechecked_on"), applied
        machine.succeed(f"git -C {repo} cat-file -e HEAD:users/alice/desktop.json")

    with subtest("Flathub is the person's, not the system's"):
        machine.wait_until_succeeds("su - alice -c 'flatpak remotes --user' | grep -q flathub", timeout=60)
        machine.fail("flatpak remotes --system | grep -q flathub")

    # Last: it locks the session.
    with subtest("a laptop that cannot wake from suspend never sleeps: the lid dims and locks, and gives it back"):
        machine.succeed("grep -qx 'HandleLidSwitch=ignore' /etc/systemd/logind.conf")
        machine.succeed("grep -qx 'AllowSuspend=no' /etc/systemd/sleep.conf")
        machine.succeed("mkdir -p /tmp/bl/panel && echo 260000 > /tmp/bl/panel/brightness")
        close = "TORQ_BACKLIGHT=/tmp/bl /etc/torq/lid-action 'button/lid LID close'"
        machine.succeed(close)
        machine.succeed("test \"$(cat /tmp/bl/panel/brightness)\" = 0")
        machine.succeed(close)  # closed twice: the brightness kept is the first one
        machine.succeed("TORQ_BACKLIGHT=/tmp/bl /etc/torq/lid-action 'button/lid LID open'")
        machine.succeed("test \"$(cat /tmp/bl/panel/brightness)\" = 260000")
  '';
}
