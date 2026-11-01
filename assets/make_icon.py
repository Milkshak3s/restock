"""Draws Restock's icon: an open box with an arrow dropping into it, on a blue tile.
Writes icon.png (256px, the window icon) and restock.ico (16-256px) to the given folder, and
optionally a strip of every size to check them.

Usage: python assets/make_icon.py assets [preview.png]"""
import sys
from pathlib import Path
from PIL import Image, ImageDraw

S = 1024
out = Path(sys.argv[1])


def draw() -> Image.Image:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # Tile with a vertical gradient, masked to a rounded square.
    grad = Image.new("RGBA", (S, S))
    top, bottom = (59, 130, 246), (30, 58, 138)
    gd = ImageDraw.Draw(grad)
    for y in range(S):
        t = y / (S - 1)
        gd.line([(0, y), (S, y)], fill=tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)) + (255,))
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle((48, 48, 976, 976), radius=210, fill=255)
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)
    # Open box: flaps, body, darker rim.
    d.polygon([(232, 560), (128, 452), (392, 452), (472, 560)], fill=(252, 211, 77))
    d.polygon([(792, 560), (896, 452), (632, 452), (552, 560)], fill=(252, 211, 77))
    d.rounded_rectangle((232, 540, 792, 880), radius=44, fill=(245, 158, 11))
    d.rectangle((232, 540, 792, 616), fill=(180, 83, 9))
    # Arrow dropping into the box.
    d.rounded_rectangle((440, 128, 584, 470), radius=28, fill=(255, 255, 255))
    d.polygon([(320, 420), (704, 420), (512, 640)], fill=(255, 255, 255))
    return img


big = draw()
out.mkdir(parents=True, exist_ok=True)
big.resize((256, 256), Image.LANCZOS).save(out / "icon.png")
sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
frames = [big.resize((n, n), Image.LANCZOS) for n in sizes]
frames[-1].save(out / "restock.ico", sizes=[(n, n) for n in sizes], append_images=frames[:-1])
if len(sys.argv) < 3:
    sys.exit()
# Preview strip for checking small sizes.
strip = Image.new("RGBA", (sum(sizes) + 10 * len(sizes), 256), (240, 240, 240, 255))
x = 0
for f in frames:
    strip.paste(f, (x, 256 - f.height), f)
    x += f.width + 10
strip.save(Path(sys.argv[2]))
