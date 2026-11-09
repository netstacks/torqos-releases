# What a first boot builds from, carried on the ISO (ADR-048 5): the inputs of every derivation that
# setup's answers or a machine's hardware can change, computed from first configurations as setup writes
# them (hosts/image/first-build/, kept so by cli/src/setup.rs's test) — two kinds of machine, two of each.
# Within a kind every derivation that differs is one a first boot builds; across kinds only those both
# have (system-path, the units), never a package only one kind has (iwd, a CPU's microcode), which is
# carried built. Their inputs that do not differ are carried, built: the build tools, firmware, iwd.
#
# The derivations are read at build time (first-build.py) and the result imported (import from
# derivation): pure evaluation reads no .drv file. So evaluating this needs a builder for `pkgs`'s
# platform — x86_64, which CI and the dev host are.
{ lib, pkgs, mkSystem, fixtures }:
let
  system = pkgs.stdenv.hostPlatform.system;
  kinds = [ "amd-wifi-1" "amd-wifi-2" "intel-1" "intel-2" ];
  # The .drv file alone, not its outputs: a drvPath's own context would make every output in its closure an
  # input, built. As a plain input the .drv comes with what it references — its whole derivation graph.
  top = d: builtins.unsafeDiscardOutputDependency (mkSystem { inherit system; configDir = fixtures + "/${d}"; }).config.system.build.toplevel.drvPath;
  graph = pkgs.runCommand "torq-first-build-graph" { nativeBuildInputs = [ pkgs.python3 ]; } ''
    python3 ${./first-build.py} ${lib.concatMapStringsSep " " top kinds} > $out
  '';
  # first-build.py splits each store path's hash (`~`), so the file refers to none; joined here.
  join = builtins.replaceStrings [ "~" ] [ "" ];
  found = builtins.fromJSON (builtins.readFile graph);
  # Each path with the context that makes it a dependency: a derivation's output, or a source.
  output = o: builtins.appendContext (join (builtins.elemAt o 2)) { ${join (builtins.elemAt o 0)} = { outputs = [ (builtins.elemAt o 1) ]; }; };
  source = q: let p = join q; in builtins.appendContext p { ${p} = { path = true; }; };
in
{
  differing = map join found.differing;
  carried = map output found.outputs ++ map source found.sources;
}
