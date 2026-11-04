# TorqOS's mark in letters (branding/terminal/torqos-logo-ascii*.txt), as fastfetch and neofetch
# draw a distribution's logo: the ring in Torque, the prompt in Foreground, each character chosen
# by how much of its cell the mark covers (":/+osy" at the edges, "hd" inside; ".:oO0NM" for the
# prompt). A terminal's cell is about twice as tall as it is wide, so the art has about twice as
# many columns as rows; `--png` renders it in a monospace font, to check by eye and by measure that
# the ring comes out round. From the mark's master (svg/mark), rasterised:
#
#   nix shell nixpkgs#librsvg -c rsvg-convert -w 1024 branding/svg/mark/torqos-mark-for-dark.svg -o /tmp/mark.png
#   nix shell --impure --expr 'with import <nixpkgs> {}; python3.withPackages (p: [ p.pillow ])' \
#     -c python3 -I branding/tools/ascii-logo.py /tmp/mark.png branding/terminal [--png out.png FONT.ttf]
import os, sys
from PIL import Image, ImageDraw, ImageFont

COLS = 46
ASPECT = 2.0  # a cell's height over its width (DejaVu Sans Mono, as most terminals: 1.95-2.1)
TORQUE, FOREGROUND = "208", "255"  # palette/colors.md: Torque, Foreground (terminal values)
RING = " .:/+osyhd"  # by coverage, light to dense; "hd" alternate where the cell is full
PROMPT = " .:oO0NM"

def cells(png):
    im = Image.open(png).convert("RGBA")
    im = im.crop(im.getchannel("A").point(lambda a: 255 if a > 40 else 0).getbbox())
    w, h = im.size
    rows = round(COLS * h / w / ASPECT)
    cw, ch = w / COLS, h / rows
    px = im.load()
    out = []
    for r in range(rows):
        line = []
        for c in range(COLS):
            x0, x1, y0, y1 = int(c * cw), int((c + 1) * cw), int(r * ch), int((r + 1) * ch)
            ring = prompt = n = 0
            for y in range(y0, y1):
                for x in range(x0, x1):
                    R, G, B, A = px[x, y]
                    n += 1
                    if A > 110:
                        if R - B > 60: ring += 1
                        else: prompt += 1
            if ring >= prompt:
                k = round(ring / n * (len(RING) - 1))
                chr_ = ("h" if c % 3 else "d") if k >= len(RING) - 2 else RING[k]
                line.append((chr_, TORQUE if chr_ != " " else None))
            else:
                k = round(prompt / n * (len(PROMPT) - 1))
                line.append((PROMPT[k], FOREGROUND if PROMPT[k] != " " else None))
        while line and line[-1][0] == " ":
            line.pop()
        out.append(line)
    while out and not out[0]: out.pop(0)
    while out and not out[-1]: out.pop()
    return out

def text(line, mode):
    s, cur = "", None
    for ch, colour in line:
        if ch != " " and colour != cur:
            if mode == "ansi": s += "\033[0;38;5;" + colour + "m"
            elif mode == "fastfetch": s += "$1" if colour == TORQUE else "$2"
            cur = colour
        s += ch
    return s + ("\033[0m" if mode == "ansi" and cur else "")

def render(art, path, font):
    f = ImageFont.truetype(font, 20)
    cw = f.getlength("M")
    asc, desc = f.getmetrics()
    lh = round(cw * ASPECT)
    W = int(cw * max(len(l) for l in art)) + 40
    im = Image.new("RGB", (W, lh * len(art) + 40), (11, 13, 16))
    d = ImageDraw.Draw(im)
    for r, line in enumerate(art):
        for c, (ch, colour) in enumerate(line):
            if ch != " ":
                d.text((20 + c * cw, 20 + r * lh), ch, font=f, fill=(255, 122, 26) if colour == TORQUE else (236, 237, 239))
    im.save(path)
    # The ring's extent, measured on the rendering: width over height, 1.0 is round.
    box = Image.eval(im.convert("L"), lambda v: 255 if v > 40 else 0).getbbox()
    return (box[2] - box[0]) / (box[3] - box[1])

if __name__ == "__main__":
    art = cells(sys.argv[1])
    out = sys.argv[2]
    for name, mode in [("torqos-logo-ascii-ansi.txt", "ansi"), ("torqos-logo-ascii-plain.txt", "plain"), ("torqos-logo-ascii-fastfetch.txt", "fastfetch")]:
        with open(os.path.join(out, name), "w") as fh:
            fh.write("\n".join(text(l, mode) for l in art) + "\n")
    if "--png" in sys.argv:
        i = sys.argv.index("--png")
        print(f"{len(art)} rows, {max(len(l) for l in art)} columns; width/height as drawn: {render(art, sys.argv[i + 1], sys.argv[i + 2]):.3f}")
