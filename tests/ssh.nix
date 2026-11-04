# NixOS VM test: SSH as TorqOS sets it up, however it is turned on (modules/services.nix,
# SECURITY-MODEL.md). With NixOS's own `services.openssh.enable` and nothing else: keys only (a
# right password is refused), only the owner may log in (another person with a key is refused), and
# root never logs in, even with a key. With passwords allowed, as setup's "password" answer does: the
# owner's password works, and an address that keeps guessing wrong is blocked (sshguard), even for a
# right key after that.
{ pkgs, self }:

let
  # The client's key: the owner's, and (to show they are refused) another person's and root's.
  key = pkgs.runCommand "torq-test-ssh-key" { nativeBuildInputs = [ pkgs.openssh ]; } ''
    mkdir $out
    ssh-keygen -q -t ed25519 -N "" -C client -f $out/id_ed25519
  '';
  pub = builtins.readFile "${key}/id_ed25519.pub";

  # A TorqOS machine with SSH turned on the plain NixOS way; `passwords`: allowed too.
  server = passwords: { lib, ... }: {
    imports = [ self.nixosModules.default ];
    torq.user = "alice";
    torq.desktop.enable = false;
    # NixOS's test instrumentation gives root an empty password; TorqOS asserts root has none.
    users.users.root.hashedPasswordFile = lib.mkForce null;
    services.openssh.enable = true;
    services.openssh.settings = lib.mkIf passwords { PasswordAuthentication = true; };
    users.users.alice.openssh.authorizedKeys.keys = [ pub ];
    users.users.bob = { isNormalUser = true; openssh.authorizedKeys.keys = [ pub ]; };
    users.users.root.openssh.authorizedKeys.keys = [ pub ];
  };
in
pkgs.testers.runNixOSTest {
  name = "torq-ssh";

  nodes.keys = server false;
  nodes.passwords = server true;
  nodes.client = { pkgs, ... }: {
    environment.systemPackages = [ pkgs.sshpass ];
  };

  testScript = ''
    start_all()
    password = "Torq-ssh-4417"
    opts = "-4 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o BatchMode=yes"

    # Each attempt ends in 30 seconds whatever happens (a connection whose packets start being dropped
    # half-way waits for ever otherwise): a test that fails says where, one that hangs does not.
    def with_key(user, host):
        return f"timeout 30 ssh {opts} -i /tmp/id_ed25519 {user}@{host} true"

    def with_password(user, host, pw):
        return f"timeout 30 sshpass -p {pw} ssh {opts.replace('BatchMode=yes', 'BatchMode=no')} -o PubkeyAuthentication=no {user}@{host} true"

    client.succeed("install -m 600 ${key}/id_ed25519 /tmp/id_ed25519")
    for m in (keys, passwords):
        m.wait_for_unit("sshd.service")
        # alice's password, as torqcli sets it: its file, outside the config repo (accounts.nix).
        m.succeed(f"(umask 077; mkpasswd -m yescrypt {password} > /var/lib/torq/secrets/passwords/alice)")
        m.succeed("/run/current-system/activate")
        # The settings sshd runs with.
        conf = m.succeed("sshd -T -f /etc/ssh/sshd_config").lower()
        assert "permitrootlogin no" in conf and "maxauthtries 3" in conf and "allowusers alice" in conf, conf
    client.wait_for_open_port(22, "keys")
    client.wait_for_open_port(22, "passwords")

    with subtest("SSH turned on, nothing else: keys only, the owner only, never root"):
        client.succeed(with_key("alice", "keys"))
        client.fail(with_password("alice", "keys", password))
        client.fail(with_key("bob", "keys"))
        client.fail(with_key("root", "keys"))
        # Nothing to guess, so nothing guarding against it.
        keys.fail("systemctl is-active sshguard")

    with subtest("passwords allowed: the owner's works, still never root's or another person's"):
        client.succeed(with_password("alice", "passwords", password))
        client.succeed(with_key("alice", "passwords"))
        client.fail(with_key("bob", "passwords"))
        client.fail(with_key("root", "passwords"))

    with subtest("an address that keeps guessing wrong is blocked, even for a right key after"):
        passwords.wait_for_unit("sshguard.service")
        for _ in range(4):
            client.fail(with_password("alice", "passwords", "wrong-guess"))
        # sshguard's block: the guesser's address in the set the firewall drops (sshd's own penalty
        # for failed logins, a few seconds' refusal, comes first and does not last).
        addr = client.succeed("ip -4 -o addr show eth1 | awk '{print $4}' | cut -d/ -f1").strip()
        passwords.wait_until_succeeds(f"${pkgs.ipset}/bin/ipset test sshguard4 {addr}", timeout=60)
        out = client.fail(with_key("alice", "passwords") + " 2>&1")
        assert "timed out" in out, out
  '';
}
