#!/usr/bin/env python3
"""Generate synthetic "ChatGPT-style" Japanese YouTube thumbnails for testing.

Only public-domain imagery is used (NASA photos shipped with scikit-image:
astronaut.png = Eileen Collins, rocket.jpg, hubble_deep_field.jpg) plus OFL fonts
from storytold/craft-fonts. Each sample is written with a ground-truth JSON file
describing every text element, so the bridge's OCR / font / style recovery can be
scored automatically.

usage: make_samples.py <image_dir> <craft_fonts_dir> <out_dir>
"""
import json
import math
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H = 1280, 720


def cover(img, w, h):
    s = max(w / img.width, h / img.height)
    img = img.resize((round(img.width * s), round(img.height * s)), Image.LANCZOS)
    x, y = (img.width - w) // 2, (img.height - h) // 2
    return img.crop((x, y, x + w, y + h))


def text_layer(canvas, truth, *, text, font_path, family, weight, size, xy, fill,
               stroke=None, shadow=None, role="title"):
    font = ImageFont.truetype(str(font_path), size)
    draw_args = dict(font=font, anchor="ls")
    if shadow:
        dx, dy, blur, color = shadow
        sh = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
        d = ImageDraw.Draw(sh)
        sw = stroke[0] if stroke else 0
        d.text((xy[0] + dx, xy[1] + dy), text, fill=color, stroke_width=sw, stroke_fill=color, **draw_args)
        if blur:
            sh = sh.filter(ImageFilter.GaussianBlur(blur))
        canvas.alpha_composite(sh)
    d = ImageDraw.Draw(canvas)
    if stroke:
        d.text(xy, text, fill=fill, stroke_width=stroke[0], stroke_fill=stroke[1], **draw_args)
    else:
        d.text(xy, text, fill=fill, **draw_args)
    bbox = d.textbbox(xy, text, stroke_width=stroke[0] if stroke else 0, **draw_args)
    truth.append({
        "role": role, "text": text, "family": family, "weight": weight, "size_px": size,
        "baseline": list(xy), "fill": "#%02x%02x%02x" % fill[:3],
        "stroke": None if not stroke else {"width": stroke[0], "color": "#%02x%02x%02x" % stroke[1][:3]},
        "shadow": None if not shadow else {"dx": shadow[0], "dy": shadow[1], "blur": shadow[2]},
        "bbox": list(bbox),
    })


def starburst(d, cx, cy, r1, r2, n, fill, outline=None):
    pts = []
    for i in range(n * 2):
        a = math.pi * i / n
        r = r1 if i % 2 == 0 else r2
        pts.append((cx + r * math.cos(a), cy + r * math.sin(a)))
    d.polygon(pts, fill=fill, outline=outline)


def bubble(d, box, tail, fill, outline):
    d.ellipse(box, fill=fill, outline=outline, width=5)
    d.polygon(tail, fill=fill, outline=outline)
    x0, y0, x1, y1 = box
    d.ellipse((x0 + 5, y0 + 5, x1 - 5, y1 - 5), fill=fill)


def person_layer(img_dir, height):
    p = Image.open(img_dir / "astronaut.png").convert("RGB")
    p = p.crop((60, 0, 400, 512))
    s = height / p.height
    return p.resize((round(p.width * s), height), Image.LANCZOS)


def sample(n, img_dir, fonts, out):
    truth = []
    gothic_b = fonts / "biz-ud-pgothic/BIZUDPGothic-Bold.ttf"
    gothic_r = fonts / "biz-ud-pgothic/BIZUDPGothic-Regular.ttf"
    mincho = fonts / "shippori-mincho/ShipporiMincho-Regular.ttf"
    if n == 1:
        bg = cover(Image.open(img_dir / "rocket.jpg").convert("RGB"), W, H).convert("RGBA")
        ov = Image.new("RGBA", (W, H), (10, 20, 60, 110))
        bg.alpha_composite(ov)
        person = person_layer(img_dir, 600)
        bg.paste(person, (W - person.width - 30, H - person.height))
        d = ImageDraw.Draw(bg)
        starburst(d, 150, 590, 120, 80, 14, (255, 220, 0, 255), (200, 40, 0, 255))
        d.rectangle((40, 430, 760, 520), fill=(255, 230, 0, 255))
        bubble(d, (780, 40, 1010, 190), [(880, 180), (930, 180), (960, 250)], (255, 255, 255, 255), (0, 0, 0, 255))
        text_layer(bg, truth, text="宇宙の謎を解明！", font_path=gothic_b, family="BIZ UDPGothic", weight=700, size=104,
                   xy=(50, 220), fill=(255, 255, 255), stroke=(9, (0, 0, 0)), shadow=(6, 6, 4, (0, 0, 0, 200)))
        text_layer(bg, truth, text="知らないと損する3つの事実", font_path=gothic_b, family="BIZ UDPGothic", weight=700, size=54,
                   xy=(60, 495), fill=(20, 20, 20), role="subtitle")
        text_layer(bg, truth, text="マジで!?", font_path=gothic_b, family="BIZ UDPGothic", weight=700, size=48,
                   xy=(810, 132), fill=(230, 30, 30), role="bubble")
        text_layer(bg, truth, text="最新", font_path=gothic_b, family="BIZ UDPGothic", weight=700, size=56,
                   xy=(94, 610), fill=(200, 20, 0), role="badge")
    elif n == 2:
        bg = Image.new("RGBA", (W, H))
        d = ImageDraw.Draw(bg)
        for y in range(H):
            t = y / H
            d.line([(0, y), (W, y)], fill=(int(250 - 60 * t), int(90 + 40 * t), int(40 + 20 * t), 255))
        for i in range(0, W + H, 60):
            d.line([(i, 0), (i - H, H)], fill=(255, 140, 60, 255), width=18)
        person = person_layer(img_dir, 640)
        bg.paste(person, (40, H - person.height))
        text_layer(bg, truth, text="たった1日で", font_path=gothic_b, family="BIZ UDPGothic", weight=700, size=88,
                   xy=(470, 180), fill=(255, 240, 0), stroke=(8, (120, 0, 0)), shadow=(5, 5, 3, (0, 0, 0, 180)))
        text_layer(bg, truth, text="人生が変わる", font_path=gothic_b, family="BIZ UDPGothic", weight=700, size=110,
                   xy=(460, 330), fill=(255, 255, 255), stroke=(10, (0, 0, 0)), shadow=(6, 6, 4, (0, 0, 0, 200)))
        d = ImageDraw.Draw(bg)
        d.rounded_rectangle((480, 420, 1220, 520), radius=20, fill=(20, 60, 160, 255))
        text_layer(bg, truth, text="プロが教える習慣術", font_path=gothic_r, family="BIZ UDPGothic", weight=400, size=60,
                   xy=(520, 493), fill=(255, 255, 255), role="subtitle")
    elif n == 3:
        bg = cover(Image.open(img_dir / "hubble_deep_field.jpg").convert("RGB"), W, H).convert("RGBA")
        person = person_layer(img_dir, 560)
        bg.paste(person, (W - person.width - 60, H - person.height))
        text_layer(bg, truth, text="星の一生", font_path=mincho, family="Shippori Mincho", weight=400, size=150,
                   xy=(60, 260), fill=(255, 255, 255), stroke=(6, (30, 60, 200)), shadow=(0, 0, 8, (120, 180, 255, 255)))
        text_layer(bg, truth, text="138億年の物語", font_path=mincho, family="Shippori Mincho", weight=400, size=64,
                   xy=(70, 380), fill=(255, 215, 120), role="subtitle")
    else:
        bg = Image.new("RGBA", (W, H), (245, 245, 240, 255))
        d = ImageDraw.Draw(bg)
        d.rectangle((0, 0, W, 140), fill=(230, 0, 50, 255))
        d.ellipse((900, 300, 1300, 700), fill=(255, 210, 0, 255))
        person = person_layer(img_dir, 560)
        bg.paste(person, (W - person.width - 40, H - person.height))
        text_layer(bg, truth, text="【完全解説】", font_path=gothic_b, family="BIZ UDPGothic", weight=700, size=80,
                   xy=(40, 105), fill=(255, 255, 255), role="badge")
        text_layer(bg, truth, text="初心者でも", font_path=gothic_b, family="BIZ UDPGothic", weight=700, size=96,
                   xy=(60, 300), fill=(20, 20, 20), stroke=(6, (255, 255, 255)))
        text_layer(bg, truth, text="すぐ分かる！", font_path=gothic_b, family="BIZ UDPGothic", weight=700, size=120,
                   xy=(60, 460), fill=(230, 0, 50), stroke=(8, (255, 255, 255)), shadow=(5, 5, 2, (0, 0, 0, 160)))
        text_layer(bg, truth, text="保存版", font_path=gothic_r, family="BIZ UDPGothic", weight=400, size=56,
                   xy=(70, 600), fill=(60, 60, 60), role="subtitle")
    out.mkdir(parents=True, exist_ok=True)
    bg.convert("RGB").save(out / f"thumb{n}.png")
    (out / f"thumb{n}.truth.json").write_text(json.dumps({"size": [W, H], "texts": truth}, ensure_ascii=False, indent=1))


def main():
    img_dir, fonts, out = map(Path, sys.argv[1:4])
    for n in (1, 2, 3, 4):
        sample(n, img_dir, fonts, out)


if __name__ == "__main__":
    main()
