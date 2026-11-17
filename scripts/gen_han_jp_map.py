#!/usr/bin/env python3
"""Build the simplified/traditional → Japanese kanji map from Unihan.

The chat reply fixer and its tests share one table, stored twice:

- src/features/chat/japaneseHanMap.ts
- src-tauri/src/llm/japanese_han_map.rs

A source character is kept only when it is a CJK ideograph that is NOT on the
Japanese allowlist (the same bitmap ``gen_japanese_han_allowlist.py`` writes)
and the variant links point at exactly one allowlisted kanji.

Links used as equivalences:

- kSimplifiedVariant, kTraditionalVariant, kZVariant (either direction)
- kJapaneseNewVariant / kJapaneseOldVariant, only to prefer the shinjitai when
  several allowlisted forms are old and new shapes of that one kanji
  (為/爲 → 為, 關 → 関). A split such as 发 → 発/髪 stays unmapped.

kSemanticVariant is read and then ignored. Unihan uses it for loose relations
that are not the same character (亲→榛, 氯→綠, 开→幵). Substituting those
would change the meaning.

When the traditional form itself is not allowlisted, one extra hop through
that form is allowed (乡 → 鄉 → 郷) and only if it still yields one target.

Run from the repository root:

    python3 scripts/gen_han_jp_map.py

The Unihan zip is cached under scripts/.cache/ (gitignored).
"""

from __future__ import annotations

import base64
import re
import urllib.request
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TS_ALLOW = ROOT / "src/features/chat/japaneseHanData.ts"
TS_OUT = ROOT / "src/features/chat/japaneseHanMap.ts"
RS_OUT = ROOT / "src-tauri/src/llm/japanese_han_map.rs"
UNIHAN_VERSION = "18.0.0"
UNIHAN_URL = f"https://www.unicode.org/Public/{UNIHAN_VERSION}/ucd/Unihan.zip"
CACHE = ROOT / "scripts/.cache" / f"Unihan-{UNIHAN_VERSION}.zip"

HAN_BMP_A0 = 0x3400
HAN_BMP_B0 = 0xF900

STRONG = ("kSimplifiedVariant", "kTraditionalVariant", "kZVariant")
SEMANTIC = "kSemanticVariant"
JP_NEW = "kJapaneseNewVariant"
JP_OLD = "kJapaneseOldVariant"
WANTED = set(STRONG) | {SEMANTIC, JP_NEW, JP_OLD}


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


def load_allowlist() -> tuple[bytes, bytes, set[int]]:
    text = TS_ALLOW.read_text()
    a = re.search(r'HAN_BMP_A =\s*"([^"]+)"', text, re.S)
    b = re.search(r'HAN_BMP_B =\s*"([^"]+)"', text, re.S)
    extra = re.search(r"HAN_EXTRA = \[(.*?)\];", text, re.S)
    if not a or not b or not extra:
        raise SystemExit("japaneseHanData.ts is missing the allowlist tables")
    bmp_a = base64.b64decode(a.group(1))
    bmp_b = base64.b64decode(b.group(1))
    nums = {int(item) for item in re.findall(r"\d+", extra.group(1))}
    return bmp_a, bmp_b, nums


def make_is_jp(bmp_a: bytes, bmp_b: bytes, extra: set[int]):
    def is_jp(cp: int) -> bool:
        if not is_cjk(cp):
            return False
        if HAN_BMP_A0 <= cp < HAN_BMP_A0 + len(bmp_a) * 8:
            index = cp - HAN_BMP_A0
            return (bmp_a[index >> 3] & (1 << (index & 7))) != 0
        if HAN_BMP_B0 <= cp < HAN_BMP_B0 + len(bmp_b) * 8:
            index = cp - HAN_BMP_B0
            return (bmp_b[index >> 3] & (1 << (index & 7))) != 0
        return cp in extra

    return is_jp


def ensure_unihan() -> Path:
    if CACHE.is_file() and CACHE.stat().st_size > 0:
        return CACHE
    CACHE.parent.mkdir(parents=True, exist_ok=True)
    print(f"downloading {UNIHAN_URL}")
    urllib.request.urlretrieve(UNIHAN_URL, CACHE)
    return CACHE


def load_variants(zip_path: Path) -> dict[int, dict[str, set[int]]]:
    import zipfile

    raw: dict[int, dict[str, set[int]]] = defaultdict(lambda: defaultdict(set))
    with zipfile.ZipFile(zip_path) as archive:
        text = archive.read("Unihan_Variants.txt").decode("utf-8")
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) < 3 or parts[1] not in WANTED:
            continue
        cp = int(parts[0][2:], 16)
        for token in parts[2].split():
            match = re.match(r"U\+([0-9A-Fa-f]+)", token)
            if match:
                raw[cp][parts[1]].add(int(match.group(1), 16))
    return raw


def build_map(raw: dict[int, dict[str, set[int]]], is_jp) -> tuple[dict[int, int], int]:
    strong: dict[int, set[int]] = defaultdict(set)
    jp_new: dict[int, set[int]] = defaultdict(set)
    semantic_rows = 0
    for cp, fields in raw.items():
        if fields.get(SEMANTIC):
            semantic_rows += 1
        for field in STRONG:
            for target in fields.get(field, ()):
                if target == cp:
                    continue
                strong[cp].add(target)
                strong[target].add(cp)
        for target in fields.get(JP_NEW, ()):
            if target != cp:
                jp_new[cp].add(target)
        for target in fields.get(JP_OLD, ()):
            if target != cp:
                jp_new[target].add(cp)

    def new_forms(cp: int) -> set[int]:
        return {target for target in jp_new.get(cp, ()) if is_jp(target) and target != cp}

    def prefer(cp: int) -> int:
        seen = {cp}
        cur = cp
        for _ in range(4):
            nxts = new_forms(cur)
            if len(nxts) != 1:
                return cur
            nxt = next(iter(nxts))
            if nxt in seen:
                return cur
            seen.add(nxt)
            cur = nxt
        return cur

    def direct(cp: int) -> set[int]:
        return {prefer(target) for target in strong.get(cp, ()) if is_jp(target)}

    mapping: dict[int, int] = {}
    ambiguous: set[int] = set()
    sources = [cp for cp in strong if is_cjk(cp) and not is_jp(cp)]
    for cp in sources:
        found = direct(cp)
        if len(found) == 1:
            mapping[cp] = next(iter(found))
        elif len(found) > 1:
            ambiguous.add(cp)

    for cp in sources:
        if cp in mapping or cp in ambiguous:
            continue
        found: set[int] = set()
        for mid in strong.get(cp, ()):
            if is_jp(mid):
                continue
            found |= direct(mid)
        found.discard(cp)
        if len(found) == 1:
            mapping[cp] = next(iter(found))

    cleaned = {
        src: dst
        for src, dst in mapping.items()
        if src != dst and is_cjk(src) and not is_jp(src) and is_jp(dst)
    }
    return cleaned, semantic_rows


def pack(mapping: dict[int, int]) -> str:
    blob = bytearray()
    for src, dst in sorted(mapping.items()):
        blob += src.to_bytes(4, "little")
        blob += dst.to_bytes(4, "little")
    return base64.b64encode(bytes(blob)).decode("ascii")


def wrap(encoded: str, width: int = 100) -> str:
    return "\n".join(encoded[i : i + width] for i in range(0, len(encoded), width))


def write_outputs(encoded: str) -> None:
    body = wrap(encoded)
    TS_OUT.write_text(
        "/* Japanese Han substitutions.\n"
        " * Generated by scripts/gen_han_jp_map.py from Unicode "
        f"{UNIHAN_VERSION} Unihan (kSimplifiedVariant,\n"
        " * kTraditionalVariant, kZVariant, then kJapaneseNewVariant to prefer shinjitai).\n"
        " * Little-endian (source, target) code point pairs. Sources are not on the\n"
        " * Japanese allowlist; each source has one allowlisted target.\n"
        " * Whitespace in the string is not part of the base64. */\n"
        "export const HAN_JP_PAIRS = `\n"
        + body
        + "\n`;\n"
    )
    RS_OUT.write_text(
        "//! Japanese Han substitutions.\n"
        "//!\n"
        f"//! Generated by `scripts/gen_han_jp_map.py` from Unicode {UNIHAN_VERSION} Unihan\n"
        "//! (`kSimplifiedVariant`, `kTraditionalVariant`, `kZVariant`, then\n"
        "//! `kJapaneseNewVariant` to prefer shinjitai). Little-endian `(source, target)`\n"
        "//! code point pairs. Sources are not on the Japanese allowlist; each source has\n"
        "//! one allowlisted target. Whitespace in the string is not part of the base64.\n"
        "\n"
        'pub const HAN_JP_PAIRS: &str = r"\n'
        + body
        + '\n";\n'
    )


def expect(mapping: dict[int, int], src: str, dst: str | None) -> None:
    got = mapping.get(ord(src))
    want = ord(dst) if dst else None
    if got != want:
        shown = chr(got) if got else None
        raise SystemExit(f"{src} → {shown}, expected {dst}")


def main() -> None:
    bmp_a, bmp_b, extra = load_allowlist()
    is_jp = make_is_jp(bmp_a, bmp_b, extra)
    raw = load_variants(ensure_unihan())
    mapping, semantic_rows = build_map(raw, is_jp)
    expect(mapping, "现", "現")
    expect(mapping, "说", "説")
    expect(mapping, "这", "這")
    expect(mapping, "门", "門")
    expect(mapping, "为", "為")
    expect(mapping, "对", "対")
    expect(mapping, "开", "開")
    expect(mapping, "关", "関")
    expect(mapping, "经", "経")
    expect(mapping, "团", "団")
    expect(mapping, "产", "産")
    expect(mapping, "发", None)
    expect(mapping, "历", None)
    expect(mapping, "复", None)
    for ch in "国学体来与个无从广干后面里会":
        if ord(ch) in mapping:
            raise SystemExit(f"{ch} is Japanese and must not be rewritten")
    encoded = pack(mapping)
    write_outputs(encoded)
    print(f"mappings: {len(mapping)}")
    print(f"semantic rows ignored: {semantic_rows}")
    print(f"wrote {TS_OUT.relative_to(ROOT)} and {RS_OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
