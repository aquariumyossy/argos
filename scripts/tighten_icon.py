from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
ICONS = ROOT / "src-tauri" / "icons"

src = ICONS / "app-icon.png"
# Prefer original if we already tightened once
original = ICONS / "app-icon-original.png"
if original.exists():
    src_img = original
else:
    src_img = src
    original.write_bytes(src.read_bytes())

im = Image.open(src_img).convert("RGB")
arr = np.array(im)
bg_color = np.array([243, 240, 232], dtype=np.int16)
diff = np.abs(arr.astype(np.int16) - bg_color).sum(axis=2)
mask = diff > 40
ys, xs = np.where(mask)
x0, x1 = int(xs.min()), int(xs.max())
y0, y1 = int(ys.min()), int(ys.max())

pad_src = 8
x0 = max(0, x0 - pad_src)
y0 = max(0, y0 - pad_src)
x1 = min(im.width - 1, x1 + pad_src)
y1 = min(im.height - 1, y1 + pad_src)
cropped = im.crop((x0, y0, x1 + 1, y1 + 1))

size = 1024
# ~2% margin each side — maximize tray readability
margin_ratio = 0.02
inner = int(size * (1 - 2 * margin_ratio))
cw, ch = cropped.size
scale = min(inner / cw, inner / ch)
nw, nh = int(round(cw * scale)), int(round(ch * scale))
scaled = cropped.resize((nw, nh), Image.Resampling.LANCZOS)

out = Image.new("RGB", (size, size), (243, 240, 232))
ox = (size - nw) // 2
oy = (size - nh) // 2
out.paste(scaled, (ox, oy))

out.save(src, "PNG")
preview = Path(__file__).resolve().parent / "argos-icon-tight-preview.png"
out.save(preview, "PNG")

arr2 = np.array(out)
diff2 = np.abs(arr2.astype(np.int16) - bg_color).sum(axis=2)
ys2, xs2 = np.where(diff2 > 40)
print(
    "new margins LRTB",
    int(xs2.min()),
    size - 1 - int(xs2.max()),
    int(ys2.min()),
    size - 1 - int(ys2.max()),
)
print(
    "fill WH",
    round((xs2.max() - xs2.min() + 1) / size, 3),
    round((ys2.max() - ys2.min() + 1) / size, 3),
)
print("saved", src)
