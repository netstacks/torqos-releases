{ lib, rustPlatform, git }:

rustPlatform.buildRustPackage {
  pname = "torq-broker";
  # One place: Cargo.toml (scripts/bump-version sets it).
  version = (lib.importTOML ./Cargo.toml).package.version;
  src = lib.cleanSource ./.;

  # Generate once with `cargo generate-lockfile` in broker/ and commit Cargo.lock.
  cargoLock.lockFile = ./Cargo.lock;

  # Integration tests drive a real git repo, single-threaded (see .cargo/config.toml).
  nativeCheckInputs = [ git ];
  dontUseCargoParallelTests = true;

  meta = {
    description = "TorqOS privileged change broker";
    mainProgram = "torq-broker";
    license = lib.licenses.asl20;
  };
}
