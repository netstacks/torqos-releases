# NixOS VM test: how a TorqOS machine starts, through a real EFI partition and its boot menu
# (ADR-033). Written before the boot-entry writer changes, so that it proves the change changed
# nothing: it passes on today's writer (NixOS's systemd-boot installer) and must pass, unchanged
# in what it asks, on Lanzaboote's.
#
# A VM test cannot build systems, so the broker's "build" is a system the VM already has (one of
# this configuration's specialisations); everything after it is real: the system profile, the
# switch, the boot entries written to the EFI partition, the firmware choosing among them, and a
# commit confirmed that a restart rolls back.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-boot";

  nodes.machine = { lib, pkgs, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "dev";
    torq.desktop.enable = false;
    users.users.root.hashedPasswordFile = lib.mkForce null;
    environment.systemPackages = [ pkgs.jq ];

    # UEFI, a disk with an EFI partition, and the machine's own boot loader on it, as the image.
    virtualisation.useBootLoader = true;
    virtualisation.useEFIBoot = true;
    boot.loader.systemd-boot.enable = true;
    boot.loader.efi.canTouchEfiVariables = true;

    # Which system this is, to tell them apart once booted.
    environment.etc."torq-test/which".text = lib.mkDefault "first";
    specialisation.next.configuration.environment.etc."torq-test/which".text = "next";
    specialisation.confirmtarget.configuration.environment.etc."torq-test/which".text = "confirm";

    # The broker's builds are systems the VM has (the test writes their paths); the rest is what a
    # machine runs: the profile set, the switch, the boot command as it is by default.
    torq.broker.rebuildCommand = ''sys=$(cat /var/lib/torq-test/next) && nix-env -p /nix/var/nix/profiles/system --set "$sys" && "$sys/bin/switch-to-configuration" switch'';
    torq.broker.buildCommand = "cat /var/lib/torq-test/confirm";
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
    environment.etc."torq-test/night-light-off.diff".text = ''
      --- a/desktop.nix
      +++ b/desktop.nix
      @@ -1 +1 @@
      -{ nightLight = true; }
      +{ nightLight = false; }
    '';
  };

  testScript = ''
    import json, re

    repo = "/var/lib/torq/config"
    machine.wait_for_unit("multi-user.target")

    def broker(args):
        return json.loads(machine.succeed(f"torq-broker --json {args}"))
    def last_entry():
        return json.loads(machine.succeed("tail -n1 /var/lib/torq/state/ledger.jsonl"))
    def booted():
        return machine.succeed("readlink -f /run/booted-system").strip()
    def which():
        return machine.succeed("cat /etc/torq-test/which").strip()
    # The boot menu as systemd-boot sees it (`bootctl list`), whichever writer made it: NixOS's
    # (`loader/entries/nixos-generation-N.conf`) or Lanzaboote's (`EFI/Linux/nixos-generation-N-….efi`).
    def menu():
        return json.loads(machine.succeed("bootctl list --json=short 2>/dev/null"))
    def generation(e):
        # A generation's own entry (not a specialisation's): its number, else None.
        m = re.match(r"nixos-generation-(\d+)(?:-[0-9a-z]+)?\.(?:conf|efi)$", e["id"])
        return int(m.group(1)) if m and "specialisation" not in e["id"] else None
    def generations():
        return sorted(g for g in map(generation, menu()) if g is not None)
    def entry(n):
        return next(e["id"] for e in menu() if generation(e) == n)
    def default_generation():
        return next((generation(e) for e in menu() if e.get("isDefault")), None)
    def restart(crash=False):
        if crash:
            machine.crash()
        else:
            machine.shutdown()
        machine.start()
        machine.wait_for_unit("multi-user.target")
        # The boot-time rollback check done (it holds the broker's lock while it runs).
        machine.wait_until_succeeds("test \"$(systemctl show -P SubState torq-confirm-boot)\" = exited", timeout=60)

    first = booted()
    nxt = machine.succeed(f"readlink -f {first}/specialisation/next").strip()
    confirm = machine.succeed(f"readlink -f {first}/specialisation/confirmtarget").strip()
    machine.succeed(f"mkdir -p /var/lib/torq-test && echo {nxt} > /var/lib/torq-test/next && echo {confirm} > /var/lib/torq-test/confirm")
    machine.succeed(f"mkdir -p {repo} && cp /etc/torq-test/desktop.nix {repo}/ && touch {repo}/flake.nix")
    machine.succeed("torq-broker init")

    with subtest("the machine starts through systemd-boot, from its EFI partition, and its menu cannot be edited"):
        out = machine.succeed("bootctl status 2>&1")
        assert re.search(r"Product: systemd-boot", out), out
        assert which() == "first", which()
        assert machine.succeed("readlink -f /nix/var/nix/profiles/system").strip() == first
        assert generations() == [1], menu()
        # A boot entry's kernel command line cannot be changed at the menu (modules/security.nix):
        # init=/bin/sh there would be a root shell without a password.
        loader = machine.succeed("cat /boot/loader/loader.conf")
        assert re.search(r"^editor\s+(0|no|false)\s*$", loader, re.M), loader

    with subtest("a commit is a new boot entry, the default, and what the machine starts"):
        p = broker("propose --diff /etc/torq-test/night-light.diff --reason 'Night light'")["seq"]
        a = broker(f"apply {p} --approved-by tester")
        assert a["kind"] == "applied", a
        assert which() == "next", which()
        assert machine.succeed("readlink -f /nix/var/nix/profiles/system").strip() == nxt
        assert generations() == [1, 2], menu()
        assert default_generation() == 2, menu()
        restart()
        assert booted() == nxt and which() == "next", (booted(), which())
        machine.succeed("torq-broker verify")

    with subtest("an older entry chosen at the menu starts the older system, once"):
        # What choosing it in the boot menu does: the firmware's one-shot entry for the next start.
        machine.succeed(f"bootctl set-oneshot {entry(1)}")
        restart()
        assert booted() == first and which() == "first", (booted(), which())
        # Chosen once: the default is still the newest, and the next start is it again.
        assert default_generation() == 2, menu()
        restart()
        assert booted() == nxt and which() == "next", (booted(), which())

    with subtest("a commit confirmed that a power cut interrupts is rolled back: the machine starts the last confirmed system"):

        p = broker("propose --diff /etc/torq-test/night-light-off.diff --reason 'Night light off'")["seq"]
        a = broker(f"apply {p} --approved-by tester --confirm 30")
        assert which() == "confirm", which()
        # Not the boot default, and not a boot entry, until it is confirmed.
        assert generations() == [1, 2] and default_generation() == 2, menu()
        restart(crash=True)
        assert booted() == nxt and which() == "next", (booted(), which())
        machine.wait_until_succeeds("tail -n1 /var/lib/torq/state/ledger.jsonl | grep -q rolled_back", timeout=120)
        e = last_entry()
        assert e["data"]["by_reboot"] and e["data"]["applied"] == a["seq"], e
        machine.succeed(f"grep -q 'nightLight = true' {repo}/desktop.nix")
        machine.succeed("torq-broker verify")
  '';
}
