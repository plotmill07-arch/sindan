#!/usr/bin/env python3
"""End-to-end GUI test of the AI Thumbnail panel through PhotoCraft's control channel.

Start PhotoCraft first, e.g.:
    photocraft --control 7878 --control-token-file out/gui-token \
        --automation-read-root samples --automation-write-root out/gui1

usage: gui_e2e.py <port> <token-file> <screenshot-dir-relative-to-write-root>

Steps (each step's screenshot is written into the automation write root):
 1. import 4 thumbnails into the panel (aiThumbnail.import, scoped read root)
 2. decompose thumbnail #1 and #2 (independent workers) and wait until both opened
 3. inspect the layer tree of the decomposed document (editable type layers)
 4. rewrite a text layer (type.edit), toggle compare with the original twice
 5. export the finished PNG (aiThumbnail.export, scoped write root) and save .pcraft/.psd
"""
import base64, json, socket, sys, time

port, token_file = int(sys.argv[1]), sys.argv[2]
shots = sys.argv[3] if len(sys.argv) > 3 else "."


class Ctl:
    def __init__(self):
        for _ in range(120):
            try:
                self.s = socket.create_connection(("127.0.0.1", port), timeout=120)
                break
            except OSError:
                time.sleep(1)
        else:
            raise SystemExit("PhotoCraft control port not reachable")
        self.f = self.s.makefile("rwb")
        self.n = 0
        tok = open(token_file).read().strip()
        r = self.call("auth", {"token": tok})
        assert r.get("ok"), r

    def call(self, method, params=None):
        self.n += 1
        self.f.write((json.dumps({"id": self.n, "method": method, "params": params or {}}) + "\n").encode())
        self.f.flush()
        while True:
            line = self.f.readline()
            if not line:
                raise SystemExit("connection closed")
            r = json.loads(line)
            if r.get("id") == self.n:
                return r

    def ok(self, method, params=None):
        r = self.call(method, params)
        if not r.get("ok"):
            raise SystemExit(f"{method} {params}: {r.get('error')}")
        return r.get("result")

    def shot(self, name):
        return self.ok("ui.screenshot", {"path": f"{name}.png" if shots in (".", "") else f"{shots}/{name}.png"})


def docs(c):
    r = c.ok("engine.execute", {"command": "session.inspect"})
    return r.get("documents", [])


def layers(c):
    d = c.ok("engine.execute", {"command": "document.inspect"})
    out = []

    def walk(ls, depth):
        for l in ls:
            out.append((depth, l))
            walk(l.get("children") or [], depth + 1)
    walk(d.get("layers", []), 0)
    return d, out


c = Ctl()
print("connected")
c.ok("engine.execute", {"command": "prefs.set", "params": {"path": "interface.language", "value": "ja"}})
c.ok("ui.resize", {"width": 1600, "height": 1000})
time.sleep(1)
# A software GPU in CI shows a one-time "switched to CPU" notice: dismiss it if present.
if c.ok("ui.inspect").get("gpuFallbackNotice"):
    c.ok("ui.click", {"x": 671, "y": 584})
r = c.ok("ui.menu.invoke", {"id": "aiThumbnail.import", "params": {"paths": ["thumb1.png", "thumb2.png", "thumb3.png", "thumb4.png"]}})
print("1. import:", r)
assert r["loaded"] == 4
time.sleep(1)
c.shot("01_import_preview")
r = c.ok("ui.menu.invoke", {"id": "aiThumbnail.decompose", "params": {"indices": [0, 1]}})
print("2. decompose started:", r)
time.sleep(0.5)
c.shot("02_decomposing")
t0 = time.time()
while len(docs(c)) < 2:
    if time.time() - t0 > 900:
        raise SystemExit("timeout waiting for the decomposed documents")
    time.sleep(2)
print(f"   both documents opened after {time.time() - t0:.1f}s")
time.sleep(2)
c.shot("03_decomposed")
d, ls = layers(c)
print("3. layers of", d.get("name"))
texts = []
for depth, l in ls:
    kind = l.get("kind") or l.get("type")
    print("  " * depth, kind, l.get("name"), "(hidden)" if l.get("visible") is False else "")
    if kind in ("Type", "text", "Text"):
        texts.append(l)
assert len(texts) >= 2, "editable type layers"
# 4. Move tool + double-click on the second title, then type a new text.
c.ok("ui.menu.invoke", {"id": "window.aiThumbnail", "params": {"open": False}})
c.ok("ui.set", {"tool": "move", "zoom": 0.5, "center": [640, 360]})
time.sleep(1)
v = c.ok("ui.inspect")["views"][0]
print("   view:", {k: v.get(k) for k in ("zoom", "center", "rect", "screenRect")})
rect = v.get("rect") or v.get("screenRect")
if rect:
    cx, cy = (rect[0] + rect[2]) / 2, (rect[1] + rect[3]) / 2
else:
    cx, cy = 657, 522
zoom = v.get("pointZoom") or v.get("zoom") or 0.5
sx, sy = cx + (700 - 640) * zoom, cy + (290 - 360) * zoom
c.ok("ui.click", {"x": sx, "y": sy, "count": 2})
time.sleep(1)
te = c.ok("ui.inspect").get("textEdit")
print("4. double-click with the Move tool ->", te)
assert te and te.get("created") is False, "double-click enters editing of the existing type layer"
c.ok("ui.key", {"key": "A", "command": True})
c.ok("ui.type", {"text": "ダブルクリックで編集"})
c.ok("ui.key", {"key": "Enter", "command": True})
time.sleep(1)
d, ls = layers(c)
names = [l.get("name") for _, l in ls if (l.get("kind") or l.get("type")) in ("Type", "Text", "text")]
print("   type layers now:", names)
assert "ダブルクリックで編集" in names
c.ok("ui.set", {"fit": True})
c.ok("ui.menu.invoke", {"id": "window.aiThumbnail", "params": {"open": True}})
time.sleep(1)
c.shot("04_text_edited")
c.ok("ui.menu.invoke", {"id": "aiThumbnail.compare"})
time.sleep(1)
c.shot("05_compare_original")
c.ok("ui.menu.invoke", {"id": "aiThumbnail.compare"})
time.sleep(1)
c.shot("06_compare_back")
r = c.ok("ui.menu.invoke", {"id": "aiThumbnail.export", "params": {"format": "png", "path": "final.png"}})
print("5. exported", r)
r = c.ok("app.save", {"path": "final.pcraft"})
print("   saved", r)
r = c.ok("app.save", {"path": "final.psd"})
print("   saved", r)
print("E2E OK")
