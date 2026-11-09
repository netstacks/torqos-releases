"""What a first boot builds from (ADR-048 5, hosts/installer/first-build.nix), at build time only.

  first-build.py carry A1 A2 B1 B2 …    the first configurations' system .drv files, two machines of each
      kind; prints JSON: the derivations that differ (`differing`), and the inputs of those that do not
      (`outputs`: [drv, output, path]; `sources`: [path]) — what the installer carries.
  first-build.py check AVAILABLE P1 P2 …  AVAILABLE: the store paths a fresh install holds (the image's
      and the carried, one a line); P…: probe machines' system .drv files. Fails, naming them, if a probe
      would fetch anything or lacks a source: what checks.first-build-offline proves.

The .drv files are inputs of the build, so their derivation graphs come with them, as references. They
are walked here (ATerm, as Nix writes it) — not with exportReferencesGraph, which adds every output in
the graph and so would build every system.
"""
import functools
import json
import re
import sys

STORE = "/nix/store/"
STR = r'"((?:[^"\\]|\\.)*)"'


@functools.cache
def parse(drv):
    """outputs {name: path}, inputs {drv: [outputs]}, sources [path]: what precedes the platform."""
    with open(drv) as f:
        text = f.read()
    m = re.match(r"Derive\(\[(.*?)\],\[(.*?)\],\[(.*?)\]," + STR + ",", text, re.S)
    if not m:
        sys.exit(f"first-build: {drv} is not a derivation as Nix writes one")
    outputs = {a: b for a, b, _, _ in re.findall(r"\(" + ",".join([STR] * 4) + r"\)", m.group(1))}
    inputs = {d: re.findall(STR, outs) for d, outs in re.findall(r"\(" + STR + r",\[(.*?)\]\)", m.group(2))}
    sources = re.findall(STR, m.group(3))
    return outputs, inputs, sources


def closure(top):
    """Every .drv `top` is built from, `top` included."""
    seen, todo = set(), [top]
    while todo:
        d = todo.pop()
        if d not in seen:
            seen.add(d)
            todo.extend(parse(d)[1])
    return seen


def name(p):
    return p[len(STORE) + 33:]


def key(p):
    return p.rsplit("/", 1)[-1]


def carry(tops):
    if len(tops) < 2 or len(tops) % 2:
        sys.exit("first-build: carry takes two machines of each kind")
    c = [closure(t) for t in tops]
    kinds = [(c[i], c[i + 1]) for i in range(0, len(c), 2)]

    def within(x, y):
        return (x - y) | (y - x)

    def across(x, y):
        ox, oy = x - y, y - x
        both = {name(p) for p in ox} & {name(p) for p in oy}
        return {p for p in ox | oy if name(p) in both}

    differ = set()
    for a, b in kinds:
        differ |= within(a, b)
    for i, (a1, a2) in enumerate(kinds):
        for b1, b2 in kinds[i + 1 :]:
            differ |= across(a1, b1) | across(a2, b2)
    outputs, sources = set(), set()
    for d in differ:
        _, inputs, srcs = parse(d)
        sources.update(srcs)
        for i, outs in inputs.items():
            if i not in differ:
                paths = parse(i)[0]
                outputs.update((i, o, paths[o]) for o in outs)

    # Each store path's hash split in two (`~`): this file is imported, and an imported file may not refer
    # to store paths, which Nix finds by their hashes. first-build.nix joins them again, with context.
    def cut(p):
        return p[: len(STORE) + 16] + "~" + p[len(STORE) + 16 :]

    json.dump({
        "differing": sorted(map(cut, differ)),
        "outputs": sorted((cut(d), o, cut(p)) for d, o, p in outputs),
        "sources": sorted(map(cut, sources)),
    }, sys.stdout, indent=1)


def check(available, tops):
    """Each probe built as Nix would: a derivation is built when an output wanted of it is not available,
    and then the outputs it wants of its inputs are wanted. A fixed-output one among them is a download."""
    have = {key(line.strip()) for line in open(available) if line.strip()}
    bad = []
    for top in tops:
        built, todo = set(), [(top, "out")]
        while todo:
            d, o = todo.pop()
            out = parse(d)[0].get(o)
            if (out and key(out) in have) or d in built:
                continue
            built.add(d)
            todo.extend((i, oo) for i, outs in parse(d)[1].items() for oo in outs)
        fetch = sorted(name(d) for d in built if re.search(r'^Derive\(\[\("out","[^"]*","[^"]+"', open(d).read()))
        missing = sorted({name(s) for d in built for s in parse(d)[2] if key(s) not in have})
        print(f"{name(top)}: {len(built)} to build, {len(fetch)} to download, {len(missing)} sources missing")
        if fetch or missing:
            bad.append(f"{name(top)} needs the network: downloads {fetch[:12]} sources {missing[:12]}")
    if bad:
        sys.exit("first-build: a first boot would need the network:\n" + "\n".join(bad))


if __name__ == "__main__":
    mode, args = sys.argv[1], sys.argv[2:]
    if mode == "carry":
        carry(args)
    elif mode == "check":
        check(args[0], args[1:])
    else:
        sys.exit(f"first-build: no mode {mode}")
