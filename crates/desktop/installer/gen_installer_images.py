# Generates the NSIS wizard branding bitmaps from the app icon:
#   sidebar.bmp  164x314  welcome/finish page brand panel (dark, gradient, wordmark)
#   header.bmp   150x57   page-header logo block (light, matches the MUI2 header band)
# Rerun after an icon/brand change:  python gen_installer_images.py
# NSIS requires 24-bit BMPs (no alpha) — PNGs are composited onto opaque bases.
import math
from PIL import Image, ImageDraw, ImageFont, ImageFilter

ICON = "../icons/icon.png"
SEGOE_SEMIBOLD = "C:/Windows/Fonts/seguisb.ttf"
SEGOE_BOLD = "C:/Windows/Fonts/segoeuib.ttf"
SEGOE = "C:/Windows/Fonts/segoeui.ttf"


def font(path_first, size):
    for p in (path_first, SEGOE_BOLD, SEGOE):
        try:
            return ImageFont.truetype(p, size)
        except OSError:
            continue
    return ImageFont.load_default()


def vgrad(w, h, top, bottom):
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        t = y / max(1, h - 1)
        px_row = tuple(round(top[i] + (bottom[i] - top[i]) * t) for i in range(3))
        for x in range(w):
            px[x, y] = px_row
    return img


def glow(base, center, radius, color, alpha):
    overlay = Image.new("RGBA", base.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(overlay)
    cx, cy = center
    d.ellipse([cx - radius, cy - radius, cx + radius, cy + radius], fill=color + (alpha,))
    overlay = overlay.filter(ImageFilter.GaussianBlur(radius / 2.2))
    base.paste(Image.alpha_composite(base.convert("RGBA"), overlay).convert("RGB"))
    return base


# ---- sidebar: 164x314, dark brand panel -------------------------------------
W, H = 164, 314
side = vgrad(W, H, (13, 17, 23), (9, 11, 16))
side = glow(side, (26, H - 30), 120, (56, 132, 255), 46)   # cool accent bloom
side = glow(side, (W - 20, 40), 90, (56, 132, 255), 22)

icon = Image.open(ICON).convert("RGBA").resize((64, 64), Image.LANCZOS)
side_rgba = side.convert("RGBA")
side_rgba.alpha_composite(icon, ((W - 64) // 2, 58))
side = side_rgba.convert("RGB")

d = ImageDraw.Draw(side)
name_f = font(SEGOE_SEMIBOLD, 30)
tw = d.textlength("Cammy", font=name_f)
d.text(((W - tw) / 2, 132), "Cammy", font=name_f, fill=(235, 240, 247))

# accent rule under the wordmark
lx = (W - 36) // 2
d.rectangle([lx, 176, lx + 36, 178], fill=(56, 132, 255))

tag_f = font(SEGOE, 13)
for i, line in enumerate(["Your cameras.", "Your data."]):
    tw = d.textlength(line, font=tag_f)
    d.text(((W - tw) / 2, 194 + i * 19), line, font=tag_f, fill=(148, 160, 178))

foot_f = font(SEGOE, 11)
foot = "Local AI · No cloud"
tw = d.textlength(foot, font=foot_f)
d.text(((W - tw) / 2, H - 28), foot, font=foot_f, fill=(96, 108, 126))

side.save("sidebar.bmp", "BMP")
side.save("sidebar-preview.png")

# ---- header: 150x57, light logo block (sits in the white MUI2 header band) ---
W, H = 150, 57
hdr = Image.new("RGB", (W, H), (255, 255, 255))
icon32 = Image.open(ICON).convert("RGBA").resize((32, 32), Image.LANCZOS)
hdr_rgba = hdr.convert("RGBA")

d = ImageDraw.Draw(hdr_rgba)
name_f = font(SEGOE_SEMIBOLD, 20)
text_w = d.textlength("Cammy", font=name_f)
total = 32 + 8 + text_w
x0 = int((W - total) / 2)
hdr_rgba.alpha_composite(icon32, (x0, (H - 32) // 2))
d.text((x0 + 40, (H - 28) / 2), "Cammy", font=name_f, fill=(24, 30, 40))
hdr = hdr_rgba.convert("RGB")
hdr.save("header.bmp", "BMP")
hdr.save("header-preview.png")
print("wrote sidebar.bmp header.bmp (+png previews)")
