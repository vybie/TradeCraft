"""Generate TradeCraft's Windows and window icons. Requires Pillow."""
from pathlib import Path
from PIL import Image, ImageDraw

root = Path(__file__).resolve().parent.parent
assets = root / "apps" / "tradecraft" / "assets"
assets.mkdir(parents=True, exist_ok=True)
scale = 4
image = Image.new("RGBA", (256 * scale, 256 * scale), (0, 0, 0, 0))
draw = ImageDraw.Draw(image)

def box(coords):
    return tuple(round(value * scale) for value in coords)

draw.rounded_rectangle(box((7, 7, 249, 249)), radius=52 * scale, fill="#151d30")
draw.rounded_rectangle(box((10, 10, 246, 246)), radius=49 * scale, outline="#344765", width=3 * scale)
draw.ellipse(box((65, 65, 191, 191)), outline="#6a9fff", width=10 * scale)
for coords in [(39, 123, 101, 133), (155, 123, 217, 133),
               (123, 39, 133, 101), (123, 155, 133, 217)]:
    draw.rounded_rectangle(box(coords), radius=5 * scale, fill="#6a9fff")
draw.ellipse(box((116, 116, 140, 140)), fill="#50c7c2")

image = image.resize((256, 256), Image.Resampling.LANCZOS)
image.save(assets / "tradecraft.png")
image.save(assets / "tradecraft.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
(assets / "tradecraft-64.rgba").write_bytes(image.resize((64, 64), Image.Resampling.LANCZOS).tobytes())
