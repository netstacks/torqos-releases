{ lib, rustPlatform, git, pam }:

rustPlatform.buildRustPackage {
  pname = "torq-broker";
  # One place: Cargo.toml (scripts/bump-version sets it).
  version = (lib.importTOML ./Cargo.toml).package.version;
  # This crate and torq-wire (ADR-044), nothing else of the repository: a change elsewhere rebuilds nothing.
  src = lib.fileset.toSource {
    root = ../.;
    # Not a build left in the working tree (a `path:` source, in development, copies it).
    fileset = lib.fileset.difference (lib.fileset.unions [ ./. ../wire ])
      (lib.fileset.unions [ (lib.fileset.maybeMissing ./target) (lib.fileset.maybeMissing ../wire/target) ]);
  };
  cargoRoot = "broker";
  buildAndTestSubdir = "broker";

  # Generate once with `cargo generate-lockfile` in broker/ and commit Cargo.lock.
  cargoLock.lockFile = ./Cargo.lock;

  # PAM: a system change's password, checked by the broker itself (ADR-040).
  buildInputs = [ pam ];

  # Integration tests drive a real git repo, single-threaded (see .cargo/config.toml).
  nativeCheckInputs = [ git ];
  dontUseCargoParallelTests = true;

  meta = {
    description = "TorqOS privileged change broker";
    mainProgram = "torq-broker";
    license = lib.licenses.asl20;
  };
}
