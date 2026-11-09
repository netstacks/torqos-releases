"""What a first boot builds from (ADR-048 5, hosts/installer/first-build.nix), at build time only.

Arguments: the four first configurations' system derivations (.drv files, inputs of this build: their
derivation graphs come with them, as references), kind A machine 1, A 2, kind B 1, B 2. Walks every .drv
they are built from (ATerm, as Nix writes it) — not exportReferencesGraph, which adds every output in
the graph and so would build all four systems — and prints JSON: the derivations that differ
(`differing`), and the inputs of those that do not (`outputs`: [drv, output, path]; `sources`: [path]).
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


def main(a1, a2, b1, b2):
    c = [closure(t) for t in (a1, a2, b1, b2)]

    def within(x, y):
        return (x - y) | (y - x)

    def across(x, y):
        ox, oy = x - y, y - x
        both = {name(p) for p in ox} & {name(p) for p in oy}
        return {p for p in ox | oy if name(p) in both}

    differ = within(c[0], c[1]) | within(c[2], c[3]) | across(c[0], c[2]) | across(c[1], c[3])
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


if __name__ == "__main__":
    main(*sys.argv[1:])
