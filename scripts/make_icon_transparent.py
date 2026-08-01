from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
ICONS = ROOT / "src-tauri" / "icons"

# Prefer the tightened master if present; else original / current
candidates = [
    ICONS / "app-icon.png",
    ICONS / "app-icon-original.png",
]
src = next(p for p in candidates if p.exists())

im = Image.open(src).convert("RGBA")
arr = np.array(im)

# Cream background ~ #f3f0e8 — make near-bg pixels transparent
bg = np.array([243, 240, 232], dtype=np.int16)
rgb = arr[:, :, :3].astype(np.int16)
dist = np.abs(rgb - bg).sum(axis=2)

# Soft edge: fully transparent when very close to bg, keep green solid
alpha = arr[:, :, 3].astype(np.float32)
# dist < 25 -> transparent; dist > 60 -> opaque; smooth between
t0, t1 = 25.0, 60.0
fade = np.clip((dist - t0) / (t1 - t0), 0.0, 1.0)
alpha = (alpha * fade).astype(np.uint8)

out = arr.copy()
out[:, :, 3] = alpha
# Zero RGB on fully transparent pixels to avoid fringe in some scalers
out[alpha == 0, :3] = 0

result = Image.fromarray(out, "RGBA")
out_path = ICONS / "app-icon.png"
result.save(out_path, "PNG")

preview = Path(__file__).resolve().parent / "argos-icon-transparent-preview.png"
# Checkerboard preview for visual check
prev = Image.new("RGBA", result.size, (0, 0, 0, 0))
check = Image.new("RGB", result.size, (200, 200, 200))
tile = 64
for y in range(0, result.size[1], tile):
    for x in range(0, result.size[0], tile):
        if ((x // tile) + (y // tile)) % 2 == 0:
            check.paste(
                (240, 240, 240),
                (x, y, min(x + tile, result.size[0]), min(y + tile, result.size[1])),
            )
prev = check.convert("RGBA")
prev.alpha_composite(result)
prev.convert("RGB").save(preview, "PNG")

opaque = int((alpha > 0).sum())
solid = int((alpha == 255).sum())
print(f"source={src}")
print(f"opaque_px={opaque} solid_px={solid} transparent_px={alpha.size - opaque}")
print(f"saved={out_path}")
