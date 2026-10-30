#!/usr/bin/env python3
# Samples for SECS seconds (default 60) the footprint and CPU of RustyBar and of SketchyBar with
# its helpers, and the processes each starts (children seen at least once in the samples).
# Usage: scripts/measure.py [PID] [SECS]; without PID, the running rustybar.
import re, subprocess, sys, time

rb = int(sys.argv[1]) if len(sys.argv) > 1 else int(subprocess.run(["pgrep", "-x", "rustybar"], capture_output=True, text=True).stdout.split()[0])
SECS = int(sys.argv[2]) if len(sys.argv) > 2 else 60

def ps():
    out = subprocess.run(["ps", "-Ao", "pid=,ppid=,time=,command="], capture_output=True, text=True).stdout
    rows = []
    for line in out.splitlines():
        m = re.match(r"\s*(\d+)\s+(\d+)\s+(\S+)\s+(.*)", line)
        if m:
            rows.append((int(m[1]), int(m[2]), m[3], m[4]))
    return rows

def secs(t):
    s = 0.0
    for x in t.split(":"):
        s = s * 60 + float(x)
    return s

def tree(rows, roots):
    ids = set(roots)
    changed = True
    while changed:
        changed = False
        for pid, ppid, _, _ in rows:
            if ppid in ids and pid not in ids:
                ids.add(pid)
                changed = True
    return ids

def sketchy(rows):
    return {pid for pid, _, _, cmd in rows if "sketchybar" in cmd and "measure.py" not in cmd}

def footprint(pids):
    total = 0
    for p in pids:
        out = subprocess.run(["footprint", str(p)], capture_output=True, text=True).stdout
        m = re.search(r"Footprint: ([\d.]+) (KB|MB|GB)", out)
        if m:
            total += float(m[1]) * {"KB": 1 / 1024, "MB": 1, "GB": 1024}[m[2]]
    return total

rows = ps()
sb0, rb0 = tree(rows, sketchy(rows)), tree(rows, {rb})
cpu0 = {pid: secs(t) for pid, _, t, _ in rows}
seen_sb, seen_rb = set(sb0), set(rb0)
t0 = time.time()
while time.time() - t0 < SECS:
    rows = ps()
    seen_sb |= tree(rows, sketchy(rows))
    seen_rb |= tree(rows, {rb})
    time.sleep(0.05)
rows = ps()
cpu1 = {pid: secs(t) for pid, _, t, _ in rows}
el = time.time() - t0
sb1, rb1 = tree(rows, sketchy(rows)), tree(rows, {rb})

def cpu(ids):
    # Only the processes alive at the end: the short-lived ones are missing.
    return sum(cpu1[p] - cpu0.get(p, 0) for p in ids if p in cpu1) / el * 100

print(f"RustyBar:   footprint {footprint(rb1):.0f} MB in {len(rb1)} process(es), CPU {cpu(rb1):.2f}%, started {len(seen_rb - rb0)} process(es) in {el:.0f}s")
if sb1:
    print(f"SketchyBar: footprint {footprint(sb1):.0f} MB in {len(sb1)} process(es), CPU {cpu(sb1):.2f}% (long-lived only), started {len(seen_sb - sb0)} process(es) in {el:.0f}s")
