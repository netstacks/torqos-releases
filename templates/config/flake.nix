# Per-machine TorqOS config repo. Lives at /var/lib/torq/config and is written ONLY by
# torq-broker after it has been initialised. Every change here is a commit + ledger entry.
#
# TorqOS's own file: the machine is its configuration tree (torqcli `show configuration`): the data
# files here (nixos/*.json, system/, users/) and the Nix files attached in it (config-files.json).
# Nothing to add here but the platform, once — no modules: everything goes in the tree.
{
  inputs = {
    # The TorqOS distribution. While developing TorqOS itself, point this at your checkout
    # instead, e.g. "path:/home/<you>/torq". Bumping it is itself a broker-approved commit.
    torq.url = "github:netstacks/torqos";
    nixpkgs.follows = "torq/nixpkgs";
  };

  outputs = { torq, ... }: {
    # Always `torq`, whatever the host is called: the broker builds this name (torq.broker.configName).
    nixosConfigurations.torq = torq.lib.mkSystem {
      # This machine's platform, set once at setup ("aarch64-linux" or "x86_64-linux").
      system = "aarch64-linux";
      configDir = ./.;
    };
  };
}
