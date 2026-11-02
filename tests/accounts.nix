# NixOS VM test: accounts are the configuration's (modules/accounts.nix), and moving a machine to
# that keeps its people's passwords. A machine from before has its passwords in /etc/shadow only
# (set with `passwd`); its first activation with TorqOS's accounts carries each into its file before
# writing the accounts, so nobody is locked out. A locked account stays locked, a file once there is
# never replaced, and from then on the files are what counts: a file removed is a password removed.
{ pkgs, self }:

pkgs.testers.runNixOSTest {
  name = "torq-accounts";

  nodes.machine = { pkgs, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "alice";
    torq.desktop.enable = false;
    # NixOS's test instrumentation gives root an empty password, for logging in by hand; the driver
    # needs none (its own root shell), and TorqOS asserts root has none (modules/accounts.nix).
    users.users.root.hashedPasswordFile = pkgs.lib.mkForce null;
    users.users.bob.isNormalUser = true;
    users.users.carol.isNormalUser = true;
  };

  testScript = ''
    machine.wait_for_unit("multi-user.target")
    pw = "/var/lib/torq/secrets/passwords"

    def shadow(user):
        return machine.succeed(f"getent shadow {user} | cut -d: -f2").strip()

    # No files yet: every account is locked (no password), as a new machine's are before setup.
    machine.succeed(f"test ! -e {pw}/alice && test ! -e {pw}/bob")
    assert shadow("alice").startswith("!"), shadow("alice")
    machine.succeed(f"stat -c %a /var/lib/torq/secrets {pw} | grep -qx 700")
    # The broker sets the passwords of these people only (not root, not a system account).
    import json
    assert json.loads(machine.succeed("cat /etc/torq/broker.json"))["people"] == ["alice", "bob", "carol"]

    # This machine has had its carry-over (at its first boot). A `passwd` now is not carried:
    machine.succeed("usermod -p '$6$late$late' bob")
    machine.succeed("/run/current-system/activate")
    machine.succeed(f"test ! -e {pw}/bob")
    assert shadow("bob").startswith("!"), shadow("bob")

    # A machine from before (no carry-over yet): alice set a password with `passwd`, bob is locked,
    # carol has a file.
    machine.succeed("rm /var/lib/torq/secrets/carried-over")
    old = "$6$saltsalt$" + "x" * 86
    machine.succeed(f"usermod -p '{old}' alice")
    machine.succeed(f"(umask 077; echo '$6$carol$kept' > {pw}/carol)")
    machine.succeed("/run/current-system/activate")

    # alice's password carried into her file, and her account still has it.
    assert machine.succeed(f"cat {pw}/alice").strip() == old
    machine.succeed(f"stat -c '%a %U' {pw}/alice | grep -qx '600 root'")
    assert shadow("alice") == old, shadow("alice")
    # bob's lock is not a password: no file, still locked.
    machine.succeed(f"test ! -e {pw}/bob")
    assert shadow("bob").startswith("!"), shadow("bob")
    # carol's file is hers: kept, and her account follows it.
    assert machine.succeed(f"cat {pw}/carol").strip() == "$6$carol$kept"
    assert shadow("carol") == "$6$carol$kept", shadow("carol")

    # Once there, a file is what counts: a later `passwd` change does not replace it, and the next
    # activation puts the account back to the file.
    machine.succeed("usermod -p '$6$other$changed' alice")
    machine.succeed("/run/current-system/activate")
    assert shadow("alice") == old, shadow("alice")
    # A file removed is a password removed: alice is locked (key only, or not at all).
    machine.succeed(f"rm {pw}/alice")
    machine.succeed("/run/current-system/activate")
    machine.succeed(f"test ! -e {pw}/alice")
    assert shadow("alice").startswith("!"), shadow("alice")
  '';
}
