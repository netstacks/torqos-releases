{ lib, stdenv, rustPlatform, symlinkJoin, git, diffutils, coreutils, gnugrep, bubblewrap, bash, openssh }:

let
  # The filters after a torqcli `show` (`| grep`, `| wc` …, cli/src/filter.rs, ADR-017) run from these
  # tools, never from PATH, each inside bubblewrap. Linux only: elsewhere no filter runs.
  filterEnv = lib.optionalAttrs stdenv.hostPlatform.isLinux {
    TORQ_FILTER_BIN = "${symlinkJoin { name = "torq-filters"; paths = [ coreutils gnugrep ]; }}/bin";
    TORQ_BWRAP = lib.getExe bubblewrap;
    # Closes every descriptor torqcli was given before bubblewrap starts (filter.rs `command`).
    TORQ_FILTER_SH = "${bash}/bin/bash";
  };
in

rustPlatform.buildRustPackage {
  pname = "torq";
  version = "0.1.1";
  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

  env = filterEnv;
  # For the dev shell (flake.nix), so `cargo test` there builds the same way.
  passthru = { inherit filterEnv; };

  # The candidate's tests read a real git repo and build diffs; a key is read by OpenSSH's
  # ssh-keygen (pkg::key_id), on the machine from its system packages.
  nativeCheckInputs = [ git diffutils openssh ];

  # torqcli, the shell (ADR-016), is this program under its own name.
  postInstall = ''
    ln -s torq $out/bin/torqcli
  '';

  meta = {
    description = "TorqOS command line (unprivileged): changes the system by proposing to torq-broker";
    mainProgram = "torq";
    license = lib.licenses.asl20;
  };
}
