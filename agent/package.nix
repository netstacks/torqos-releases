{ lib, stdenv, rustPlatform, symlinkJoin, curl, pciutils, usbutils, iproute2, iw, util-linux, kmod, procps, coreutils }:

let
  # The programs doctor mode runs (agent/src/tools.rs), by their store paths, never from PATH: curl
  # for the model (agent/src/model.rs), the hardware tools that only report. journalctl and torqcli
  # are the system's own (/run/current-system/sw/bin), matching its journal and its broker.
  # Linux only, as torqcli's filters are.
  doctorEnv = lib.optionalAttrs stdenv.hostPlatform.isLinux {
    TORQ_CURL = lib.getExe curl;
    TORQ_DOCTOR_BIN = "${symlinkJoin { name = "torq-doctor-tools"; paths = [ pciutils usbutils iproute2 iw util-linux kmod procps coreutils ]; }}/bin";
  };
in

rustPlatform.buildRustPackage {
  pname = "torq-agent";
  # One place: Cargo.toml (scripts/bump-version sets it).
  version = (lib.importTOML ./Cargo.toml).package.version;
  src = lib.cleanSource ./.;
  cargoLock.lockFile = ./Cargo.lock;

  env = doctorEnv;
  # For the dev shell (flake.nix), so `cargo test` there builds the same way.
  passthru = { inherit doctorEnv; };

  meta = {
    description = "TorqOS agent (unprivileged, as torq-agent): doctor mode, read-only tools, proposes nothing";
    mainProgram = "torq-doctor";
    license = lib.licenses.asl20;
  };
}
