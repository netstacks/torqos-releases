# What a first boot builds from, carried on the ISO (ADR-048 5): the inputs of every derivation that
# setup's answers or a machine's hardware can change, computed from first configurations as setup writes
# them (hosts/image/first-build/, kept so by cli/src/setup.rs's test) — three kinds of machine, two of
# each, which between them give every answer setup offers. Within a kind every derivation that differs is
# one a first boot builds; across kinds only those both have (system-path, the units), never a package
# only one kind has (iwd, sshguard, a CPU's microcode), which is carried built. Their inputs that do not
# differ are carried, built: the build tools, firmware, iwd, sshguard.
#
# `check`: probe machines that are none of the kinds (the owner's Zenbook as setup made it, other mixes, a
# VM) build from the image and what is carried with nothing to download — checks.first-build-offline.
#
# The derivations are read at build time (first-build.py) and the result imported (import from
# derivation): pure evaluation reads no .drv file. So evaluating this needs a builder for `pkgs`'s
# platform — x86_64, which CI and the dev host are.
{ lib, pkgs, mkSystem, fixtures }:
let
  system = pkgs.stdenv.hostPlatform.system;
  kinds = [ "amd-wifi-1" "amd-wifi-2" "intel-1" "intel-2" "intel-wifi-pw-1" "intel-wifi-pw-2" ];
  probes = [ "probe-zenbook" "probe-amd-desk" "probe-intel-laptop" "probe-vm" ];
  # The .drv file alone, not its outputs: a drvPath's own context would make every output in its closure an
  # input, built. As a plain input the .drv comes with what it references — its whole derivation graph.
  top = d: builtins.unsafeDiscardOutputDependency (mkSystem { inherit system; configDir = fixtures + "/${d}"; }).config.system.build.toplevel.drvPath;
  graph = pkgs.runCommand "torq-first-build-graph" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    python3 ${./first-build.py} carry ${lib.concatMapStringsSep " " top kinds} > $out
  '';
  # first-build.py splits each store path's hash (`~`), so the file refers to none; joined here.
  join = builtins.replaceStrings [ "~" ] [ "" ];
  found = builtins.fromJSON (builtins.readFile graph);
  # Each path with the context that makes it a dependency: a derivation's output, or a source.
  output = o: builtins.appendContext (join (builtins.elemAt o 2)) { ${join (builtins.elemAt o 0)} = { outputs = [ (builtins.elemAt o 1) ]; }; };
  source = q: let p = join q; in builtins.appendContext p { ${p} = { path = true; }; };
  carried = map output found.outputs ++ map source found.sources;
in
{
  differing = map join found.differing;
  inherit carried;
  # `image`: the image's system, whose closure a fresh install's store is (with what is carried).
  check = image: pkgs.runCommand "torq-first-build-offline" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    python3 ${./first-build.py} check ${pkgs.closureInfo { rootPaths = [ image ] ++ carried; }}/store-paths \
      ${lib.concatMapStringsSep " " top probes}
    touch $out
  '';
}
