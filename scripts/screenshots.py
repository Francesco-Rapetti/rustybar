#!/usr/bin/env python3
# The README screenshots: each widget with its popup open under it, and the right end of the bar,
# taken window by window (so nothing behind them shows) and put on a plain gradient. Run it with a
# RustyBar started on made-up data, in English, with every widget in [layout], e.g.
#   open -n target/RustyBar.app --env RUSTYBAR_CONFIG_DIR=<dir> --env RUSTYBAR_MOCK=$PWD/examples/mock \
#     --args -AppleLanguages '(en)' -AppleLocale en_US
# and the mouse on the main display, where the popups open. Needs Pillow and swiftc.
# Usage: scripts/screenshots.py [OUT_DIR] [WIDGET…]; by default docs/screenshots, every widget.
import os, subprocess, sys, tempfile, time
from PIL import Image, ImageDraw

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RB = os.path.join(ROOT, "target/release/rustybar")
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "docs/screenshots")
MARGIN = 24
# The widgets of the right end of the bar in the header image, as in the default config
HEADER = ["audio", "network", "events", "system", "battery", "clock", "notifications"]

# CGWindowList has no command line tool: a few lines of Swift list the windows of a process
SWIFT = """
import CoreGraphics
let pid = Int32(CommandLine.arguments[1])!
let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as! [[String: Any]]
for w in list where (w[kCGWindowOwnerPID as String] as? Int32) == pid {
    let b = w[kCGWindowBounds as String] as! [String: Double]
    print(w[kCGWindowNumber as String]!, Int(b["X"]!), Int(b["Y"]!), Int(b["Width"]!), Int(b["Height"]!))
}
"""
TMP = tempfile.mkdtemp()
LISTER = os.path.join(TMP, "windows")
with open(LISTER + ".swift", "w") as f:
    f.write(SWIFT)
subprocess.run(["swiftc", "-O", "-o", LISTER, LISTER + ".swift"], check=True)
PID = subprocess.run(["pgrep", "-f", "RustyBar.app/Contents/MacOS"], capture_output=True, text=True).stdout.split()[0]

def windows():
    out = subprocess.run([LISTER, PID], capture_output=True, text=True).stdout
    return [tuple(int(x) for x in l.split()) for l in out.splitlines()]

def grab(wid):
    path = os.path.join(TMP, f"{wid}.png")
    subprocess.run(["screencapture", "-x", "-o", f"-l{wid}", path], check=True)
    return Image.open(path).convert("RGBA")

def query():
    """The widgets on the main display: name → x, y, width, height."""
    out = subprocess.run([RB, "msg", "query"], capture_output=True, text=True).stdout
    rows = [l.split() for l in out.splitlines() if l.startswith("  ")]
    main = rows[0][:-3] if rows else []
    pos = {}
    for f in rows:
        if f[:-3] == main:
            (x, y), (w, h) = map(int, f[-2].split(",")), map(int, f[-1].split("x"))
            pos[f[-3]] = (x, y, w, h)
    return pos

def background(w, h):
    top, bottom = (0x58, 0x5B, 0x70), (0x3A, 0x3C, 0x54)
    im = Image.new("RGBA", (w, h))
    d = ImageDraw.Draw(im)
    for y in range(h):
        t = y / max(h - 1, 1)
        d.line([(0, y), (w, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bottom)) + (255,))
    return im

pos = query()
x0, _, w0, _ = next(iter(pos.values()))
# The bar of the main display: the window that spans it, with the first widget in it
bar_win = next(w for w in windows() if w[1] <= x0 and x0 + w0 <= w[1] + w[3] and w[4] <= 60)
bars = {w[0] for w in windows() if w[4] == bar_win[4]}
bar = grab(bar_win[0])
_, bx, by, bw, _ = bar_win

def save(name, left, top, right, bottom, pills, popup=None):
    """The canvas from left to right on the screen, with the bar from pills[0] to pills[1] only, so
    no edge of the widgets next to it shows."""
    canvas = background(right - left, bottom - top)
    canvas.alpha_composite(bar.crop((pills[0] - bx, 0, pills[1] - bx, bar.height)), (pills[0] - left, by - top))
    if popup:
        img, px, py = popup
        canvas.alpha_composite(img, (px - left, py - top))
    canvas.convert("RGB").save(os.path.join(OUT, f"{name}.png"), optimize=True)

os.makedirs(OUT, exist_ok=True)
for name in sys.argv[2:] or list(pos):
    x, y, w, h = pos[name]
    subprocess.run([RB, "msg", "popup", name], capture_output=True)
    time.sleep(1.6)
    # A popup hangs under its widget; another window is one still closing
    extra = [v for v in windows() if v[0] not in bars and v[1] < x + w and v[1] + v[3] > x and v[2] < y + h + 60]
    popup = None
    left, top, right, bottom = x, y, x + w, y + h
    if extra:
        wid, px, py, pw, ph = extra[0]
        popup = (grab(wid), px, py)
        left, right, bottom = min(left, px), max(right, px + pw), max(bottom, py + ph)
        subprocess.run([RB, "msg", "popup", name], capture_output=True)
        time.sleep(1.0)
    pills = (max(x - 6, bx), min(x + w + 6, bx + bw))
    save(name, max(left - MARGIN, bx), top - MARGIN // 2, min(right + MARGIN, bx + bw), bottom + MARGIN, pills, popup)
    print(name, "with its popup" if popup else "")

shown = [pos[n] for n in HEADER if n in pos]
if shown:
    left = min(p[0] for p in shown) - MARGIN
    top = min(p[1] for p in shown) - MARGIN // 2
    save("bar", left, top, bx + bw, top + 28 + MARGIN, (left + MARGIN - 6, bx + bw))
    print("bar")
