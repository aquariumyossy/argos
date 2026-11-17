#!/usr/bin/env python3
"""Fill the Japanese Han allowlist from the Python codecs.

The chat and the write-tool check share one allowlist, stored twice so the
screen can test a reply without a round trip:

- src/features/chat/japaneseHanData.ts
- src-tauri/src/llm/japanese_han_data.rs

This script reads the TypeScript file, then turns on every CJK ideograph that
Python's codecs decode:

- ``shift_jis``: JIS X 0208 kanji (6,356 ideographs in CPython)
- ``cp932``: Windows-31J, which adds the NEC and IBM extension kanji
  (including U+9AD9 髙)

U+20BB7 (𠮷, tsuchi-yoshi) is not in either codec, so it is added by hand.
Simplified forms that those codecs do not contain, such as 们这说话门问题,
stay off.

Run from the repository root:

    python3 scripts/gen_japanese_han_allowlist.py
"""

from __future__ import annotations

import base64
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TS_PATH = ROOT / "src/features/chat/japaneseHanData.ts"
RS_PATH = ROOT / "src-tauri/src/llm/japanese_han_data.rs"

HAN_BMP_A0 = 0x3400
HAN_BMP_B0 = 0xF900
# 𠮷. Neither shift_jis nor cp932 emits this scalar.
TSUCHI_YOSHI = 0x20BB7


def is_cjk(cp: int) -> bool:
    return (
        0x3400 <= cp <= 0x4DBF
        or 0x4E00 <= cp <= 0x9FFF
        or 0xF900 <= cp <= 0xFAFF
        or 0x20000 <= cp <= 0x2A6DF
        or 0x2A700 <= cp <= 0x2B73F
        or 0x2B740 <= cp <= 0x2B81F
        or 0x2B820 <= cp <= 0x2CEAF
        or 0x2CEB0 <= cp <= 0x2EBEF
        or 0x30000 <= cp <= 0x3134F
        or 0x31350 <= cp <= 0x323AF
    )


def collect(encoding: str) -> set[int]:
    points: set[int] = set()
    leads = list(range(0x81, 0xA0)) + list(range(0xE0, 0xFD))
    seconds = list(range(0x40, 0x7F)) + list(range(0x80, 0xFD))
    for lead in leads:
        for second in seconds:
            try:
                text = bytes((lead, second)).decode(encoding)
            except UnicodeDecodeError:
                continue
            if len(text) != 1:
                continue
            cp = ord(text)
            if is_cjk(cp):
                points.add(cp)
    return points


def bit_at(table: bytearray, index: int) -> bool:
    return (table[index >> 3] & (1 << (index & 7))) != 0


def set_bit(table: bytearray, index: int) -> None:
    table[index >> 3] |= 1 << (index & 7)


def load_ts(text: str) -> tuple[bytearray, bytearray, list[int]]:
    a = re.search(r'HAN_BMP_A =\s*"([^"]+)"', text, re.S)
    b = re.search(r'HAN_BMP_B =\s*"([^"]+)"', text, re.S)
    extra = re.search(r"HAN_EXTRA = \[(.*?)\];", text, re.S)
    if not a or not b or not extra:
        raise SystemExit("japaneseHanData.ts is missing HAN_BMP_A, HAN_BMP_B, or HAN_EXTRA")
    bmp_a = bytearray(base64.b64decode(a.group(1)))
    bmp_b = bytearray(base64.b64decode(b.group(1)))
    nums = [int(item) for item in re.findall(r"\d+", extra.group(1))]
    return bmp_a, bmp_b, nums


def already(cp: int, bmp_a: bytearray, bmp_b: bytearray, extra: set[int]) -> bool:
    if HAN_BMP_A0 <= cp < HAN_BMP_A0 + len(bmp_a) * 8:
        return bit_at(bmp_a, cp - HAN_BMP_A0)
    if HAN_BMP_B0 <= cp < HAN_BMP_B0 + len(bmp_b) * 8:
        return bit_at(bmp_b, cp - HAN_BMP_B0)
    return cp in extra


def allow(cp: int, bmp_a: bytearray, bmp_b: bytearray, extra: set[int]) -> bool:
    """True when this code point was newly turned on."""
    if already(cp, bmp_a, bmp_b, extra):
        return False
    if HAN_BMP_A0 <= cp < HAN_BMP_A0 + len(bmp_a) * 8:
        set_bit(bmp_a, cp - HAN_BMP_A0)
        return True
    if HAN_BMP_B0 <= cp < HAN_BMP_B0 + len(bmp_b) * 8:
        set_bit(bmp_b, cp - HAN_BMP_B0)
        return True
    extra.add(cp)
    return True


def wrap_numbers(nums: list[int]) -> str:
    lines: list[str] = []
    line = "  "
    for index, num in enumerate(nums):
        piece = str(num) + ("," if index + 1 < len(nums) else "")
        if len(line) + len(piece) + 1 > 100:
            lines.append(line.rstrip())
            line = "  " + piece + " "
        else:
            line += piece + " "
    lines.append(line.rstrip())
    return "\n".join(lines)


def write_ts(bmp_a: bytes, bmp_b: bytes, extra: list[int]) -> None:
    a = base64.b64encode(bmp_a).decode("ascii")
    b = base64.b64encode(bmp_b).decode("ascii")
    body = f"""/* Japanese Han allowlist.
 * Base bitmap: LexCrew-Doc japaneseHanData.ts (Joyo, jinmeiyo, JIS/vendor).
 * Additions: scripts/gen_japanese_han_allowlist.py unions Python's shift_jis
 * (JIS X 0208 kanji) and cp932 (NEC and IBM extension kanji, including U+9AD9),
 * plus U+20BB7 (𠮷), which neither codec emits. */
export const HAN_BMP_A0 = 0x3400;
export const HAN_BMP_B0 = 0xf900;
export const HAN_BMP_A =
  "{a}";
export const HAN_BMP_B =
  "{b}";
export const HAN_EXTRA = [
{wrap_numbers(extra)}
];
"""
    TS_PATH.write_text(body)


def write_rs(bmp_a: bytes, bmp_b: bytes, extra: list[int]) -> None:
    a = base64.b64encode(bmp_a).decode("ascii")
    b = base64.b64encode(bmp_b).decode("ascii")
    numbers = "\n".join(
        f"    {line.strip()}" if line.strip() else line for line in wrap_numbers(extra).splitlines()
    )
    body = f"""//! Japanese Han allowlist bitmaps.
//!
//! The base bitmap is LexCrew-Doc `japaneseHanData.ts`.
//! `scripts/gen_japanese_han_allowlist.py` turns on every CJK ideograph from
//! Python's `shift_jis` (JIS X 0208) and `cp932` (NEC and IBM extensions,
//! including U+9AD9 髙), and U+20BB7 𠮷, which neither codec emits.

pub const HAN_BMP_A0: u32 = 0x3400;
pub const HAN_BMP_B0: u32 = 0xf900;
pub const HAN_BMP_A: &str = r"{a}";
pub const HAN_BMP_B: &str = r"{b}";
pub const HAN_EXTRA: &[u32] = &[
{numbers}
];
"""
    RS_PATH.write_text(body)


def main() -> None:
    bmp_a, bmp_b, extra_list = load_ts(TS_PATH.read_text())
    extra = set(extra_list)
    jis = collect("shift_jis")
    cp932 = collect("cp932")
    required = jis | cp932 | {TSUCHI_YOSHI}
    added = [cp for cp in sorted(required) if allow(cp, bmp_a, bmp_b, extra)]
    write_ts(bytes(bmp_a), bytes(bmp_b), sorted(extra))
    write_rs(bytes(bmp_a), bytes(bmp_b), sorted(extra))
    print(f"shift_jis kanji: {len(jis)}")
    print(f"cp932 kanji: {len(cp932)}")
    print(f"added code points: {len(added)}")
    print("added:", " ".join(f"U+{cp:04X}" for cp in added))


if __name__ == "__main__":
    main()
