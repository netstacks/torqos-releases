# The TorqOS image as the ISO installer carries it (ADR-031): the same disk, raw, compressed, with
# its size and SHA-256 once decompressed, which `torq install` checks as it writes and reads back.
# `image`: the image's disk (a NixOS system's system.build.torqDisk, hosts/image/disk.nix).
{ pkgs, image }:

pkgs.runCommand "torqos-installer-image-${pkgs.stdenv.hostPlatform.system}" { nativeBuildInputs = [ pkgs.qemu-utils pkgs.zstd ]; } ''
  mkdir $out
  qemu-img convert -O raw ${image}/*.qcow2 image.raw
  stat -c %s image.raw > $out/size
  sha256sum image.raw | cut -d' ' -f1 > $out/sha256
  zstd -q -T$NIX_BUILD_CORES -19 image.raw -o $out/image.raw.zst
  rm image.raw
''
