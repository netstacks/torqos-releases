{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "torq-secrets";
  # One place: Cargo.toml (scripts/bump-version sets it).
  version = (lib.importTOML ./Cargo.toml).package.version;
  # This crate and torq-wire (ADR-044), nothing else of the repository.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.difference (lib.fileset.unions [ ./. ../wire ])
      (lib.fileset.unions [ (lib.fileset.maybeMissing ./target) (lib.fileset.maybeMissing ../wire/target) ]);
  };
  cargoRoot = "secrets";
  buildAndTestSubdir = "secrets";
  cargoLock.lockFile = ./Cargo.lock;

  meta = {
    description = "TorqOS's secrets daemon: a secret used in an HTTPS request, never read (ADR-052)";
    mainProgram = "torq-secrets";
    license = lib.licenses.asl20;
  };
}
