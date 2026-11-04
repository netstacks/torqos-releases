# TorqOS's text logos, for terminals (branding/terminal/torqos-{logo,lockup}-*.txt): the
# mark, drawn in half blocks (each character two square pixels, ▀ over ▄) in the palette's
# terminal colours — alone (ANSI, plain, fastfetch's), and beside the wordmark and tagline, centred
# on it (the lockup). From the mark's master (svg/mark), rasterised:
#
#   nix shell nixpkgs#librsvg -c rsvg-convert -w 512 branding/svg/mark/torqos-mark-for-dark.svg -o /tmp/mark.png
#   nix shell --impure --expr 'with import <nixpkgs> {}; python3.withPackages (p: [ p.pillow ])' \
#     -c python3 -I branding/tools/text-logos.py /tmp/mark.png branding/terminal
#
# 28 columns of mark: the ring's arrowhead still shows, and beside the wordmark it is 77 columns,
# inside an 80-column console.
import os, sys
from PIL import Image

COLS = 28
TORQUE, FOREGROUND = "208", "255"  # palette/colors.md: Torque, Foreground (terminal values)

def cells(png):
    im = Image.open(png).convert("RGBA")
    im = im.crop(im.getchannel("A").point(lambda a: 255 if a > 40 else 0).getbbox())
    w, h = im.size
    rows = round(h * COLS / w)
    rows += rows % 2
    im = im.resize((COLS, rows), Image.LANCZOS)
    def colour(p):
        r, g, b, a = p
        return None if a < 110 else (TORQUE if r - b > 60 else FOREGROUND)
    out = []
    for y in range(0, rows, 2):
        line = []
        for x in range(COLS):
            t, u = colour(im.getpixel((x, y))), colour(im.getpixel((x, y + 1)))
            if t is None and u is None: line.append((" ", None, None))
            elif t == u: line.append(("█", t, None))
            elif u is None: line.append(("▀", t, None))
            elif t is None: line.append(("▄", u, None))
            else: line.append(("▀", t, u))
        while line and line[-1][0] == " ":
            line.pop()
        out.append(line)
    while out and not out[-1]:
        out.pop()
    return out

def text(line, ansi):
    s, cur = "", (None, None)
    for ch, fg, bg in line:
        if ansi and ch != " " and (fg, bg) != cur:
            s += "\033[0;38;5;" + fg + (";48;5;" + bg if bg else "") + "m"
            cur = (fg, bg)
        elif ansi and ch == " " and cur[1]:
            s += "\033[0m"
            cur = (None, None)
        s += ch
    return s + ("\033[0m" if ansi and cur != (None, None) else "")

def lockup(mark, word, ansi):
    top = (len(mark) - len(word)) // 2
    out = []
    for i, m in enumerate(mark):
        w = word[i - top] if 0 <= i - top < len(word) else ""
        out.append((text(m, ansi) + " " * (COLS - len(m)) + ("    " + w if w else "")).rstrip())
    return "\n".join(out) + "\n"

def fastfetch(mark):
    # fastfetch's logo file: $1 and $2 are its two colours (fastfetch-config.jsonc: 1 Torque, 2
    # Foreground), foreground only — a cell with both halves coloured keeps its top's.
    lines = []
    for line in mark:
        s, cur = "", None
        for ch, fg, bg in line:
            if ch != " " and fg != cur:
                s += "$1" if fg == TORQUE else "$2"
                cur = fg
            s += ch
        lines.append(s)
    return "\n".join(lines) + "\n"

png, terminal = sys.argv[1], sys.argv[2]
mark = cells(png)
assert not any(bg for line in mark for _, _, bg in line), "a cell with two colours: fastfetch would lose one"
def write(name, s):
    with open(os.path.join(terminal, name), "w") as f:
        f.write(s)
write("torqos-logo-fastfetch.txt", fastfetch(mark))
for kind, ansi in (("ansi", True), ("plain", False)):
    write(f"torqos-logo-{kind}.txt", "\n".join(text(m, ansi) for m in mark) + "\n")
    word = open(os.path.join(terminal, f"torqos-wordmark-{kind}.txt")).read().rstrip("\n").split("\n")
    write(f"torqos-lockup-{kind}.txt", lockup(mark, [w.rstrip() for w in word], ansi))
