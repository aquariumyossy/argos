#!/usr/bin/env python3
"""Build the simplified/traditional → Japanese kanji map from Unihan.

The chat reply fixer and its tests share one table, stored twice:

- src/features/chat/japaneseHanMap.ts
- src-tauri/src/llm/japanese_han_map.rs

A source character is kept only when it is a CJK ideograph that is NOT on the
Japanese allowlist (the same bitmap ``gen_japanese_han_allowlist.py`` writes)
and the variant links point at exactly one allowlisted kanji.

The traditional form (kTraditionalVariant) is the target when it is already
on the Japanese allowlist (谭 → 譚, 岗 → 崗, 箫 → 簫, 鲶 → 鯰). It is replaced
by a jōyō or jinmeiyō character only when that character is the shinjitai of
the same traditional character: kJapaneseNewVariant / kZVariant, or a mutual
kSemanticVariant whose gloss and Japanese readings are the same word
(綫 → 線, 舉 → 挙, 鑒 → 鑑, 淨 → 浄, 賬 → 帳). A merely similar character
(譚 → 談, 崗 → 岡, 簫 → 焼, 鯰 → 鮎) is not a target. When nothing safe is
Japanese, the source stays unmapped and the reply fixer treats it as residue.

A split such as 发 → 発/髪 stays unmapped. A loose semantic link is not a new
source: 关 stays 関 via kJapaneseNewVariant, and is not pulled toward 寡.

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
WANTED = set(STRONG) | {SEMANTIC, "kSpoofingVariant", JP_NEW, JP_OLD}


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


def load_sets(zip_path: Path) -> tuple[set[int], set[int], set[int]]:
    import zipfile

    joyo: set[int] = set()
    jinmeiyo: set[int] = set()
    gb0: set[int] = set()
    with zipfile.ZipFile(zip_path) as archive:
        text = archive.read("Unihan_OtherMappings.txt").decode("utf-8")
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) < 2:
            continue
        cp = int(parts[0][2:], 16)
        if parts[1] == "kJoyoKanji":
            joyo.add(cp)
        elif parts[1] == "kJinmeiyoKanji":
            jinmeiyo.add(cp)
        elif parts[1] == "kGB0":
            gb0.add(cp)
    return joyo, jinmeiyo, gb0


def load_word_data(zip_path: Path):
    """Glosses and Japanese readings used to tell a shinjitai from a similar word."""
    import zipfile

    on: dict[int, set[str]] = defaultdict(set)
    kun: dict[int, set[str]] = defaultdict(set)
    defin: dict[int, str] = {}
    matt: dict[int, set[str]] = defaultdict(set)
    lau: dict[int, set[str]] = defaultdict(set)
    with zipfile.ZipFile(zip_path) as archive:
        for line in archive.read("Unihan_Readings.txt").decode("utf-8").splitlines():
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 3:
                continue
            cp = int(parts[0][2:], 16)
            if parts[1] == "kJapaneseOn":
                on[cp] = set(parts[2].split())
            elif parts[1] == "kJapaneseKun":
                kun[cp] = set(parts[2].split())
            elif parts[1] == "kDefinition":
                defin[cp] = parts[2]
        for line in archive.read("Unihan_DictionaryIndices.txt").decode("utf-8").splitlines():
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 3:
                continue
            cp = int(parts[0][2:], 16)
            if parts[1] == "kMatthews":
                matt[cp] = set(parts[2].split())
            elif parts[1] == "kLau":
                lau[cp] = set(parts[2].split())
    return on, kun, defin, matt, lau


def gloss_words(text: str) -> set[str]:
    words: set[str] = set()
    for word in re.findall(r"[A-Za-z]{3,}", text.lower()):
        # "chickens" and "chicken" are the same gloss word.
        if word.endswith("s") and len(word) > 4:
            word = word[:-1]
        words.add(word)
    return words


def gloss_jaccard(defin: dict[int, str], left: int, right: int) -> float:
    words_l = gloss_words(defin.get(left, ""))
    words_r = gloss_words(defin.get(right, ""))
    if not words_l or not words_r:
        return 0.0
    return len(words_l & words_r) / len(words_l | words_r)


def gloss_related(defin: dict[int, str], left: int, right: int) -> bool:
    words_l = gloss_words(defin.get(left, ""))
    words_r = gloss_words(defin.get(right, ""))
    if not words_l or not words_r or not (words_l & words_r):
        return False
    if words_l <= words_r or words_r <= words_l:
        return True
    return len(words_l & words_r) / len(words_l | words_r) >= 0.5


def build_map(
    raw: dict[int, dict[str, set[int]]],
    is_jp,
    joyo: set[int],
    jinmeiyo: set[int],
    on: dict[int, set[str]],
    kun: dict[int, set[str]],
    defin: dict[int, str],
    matt: dict[int, set[str]],
    lau: dict[int, set[str]],
) -> tuple[dict[int, int], int, set[int]]:
    sem: dict[int, set[int]] = defaultdict(set)
    jp_new: dict[int, set[int]] = defaultdict(set)
    zvar: dict[int, set[int]] = defaultdict(set)
    trad_of: dict[int, set[int]] = defaultdict(set)
    simp_of: dict[int, set[int]] = defaultdict(set)
    semantic_rows = 0
    for cp, fields in raw.items():
        if fields.get(SEMANTIC):
            semantic_rows += 1
        for target in fields.get(SEMANTIC, ()):
            if target != cp and cp in raw.get(target, {}).get(SEMANTIC, ()):
                sem[cp].add(target)
        for target in fields.get(JP_NEW, ()):
            if target != cp:
                jp_new[cp].add(target)
        for target in fields.get(JP_OLD, ()):
            if target != cp:
                jp_new[target].add(cp)
        for target in fields.get("kZVariant", ()):
            if target != cp:
                zvar[cp].add(target)
                zvar[target].add(cp)
        for target in fields.get("kTraditionalVariant", ()):
            if target != cp:
                trad_of[cp].add(target)
                simp_of[target].add(cp)
        for target in fields.get("kSimplifiedVariant", ()):
            if target != cp:
                simp_of[cp].add(target)
                trad_of[target].add(cp)

    def same_glyph(left: int, right: int) -> bool:
        """Mutual semantic neighbors that are one character, not two similar words."""
        jac = gloss_jaccard(defin, left, right)
        on_eq = bool(on[left]) and on[left] == on[right]
        kun_l, kun_r = kun[left], kun[right]
        related = gloss_related(defin, left, right)
        # Identical gloss: 綫/線, 鑒/鑑, 舉/擧, 腳/脚.
        if jac == 1.0:
            return True
        # 凈/淨 share a Matthews headword and most of the gloss. Their Japanese
        # readings differ, so a shared reading is not required here.
        if jac >= 0.5 and (matt[left] & matt[right]):
            return True
        # Same readings and the same word, even when the English gloss is
        # worded differently (雞/鷄, 鄰/隣). 崗/岡 share the readings but not
        # a gloss word, so they stay apart.
        shared = gloss_words(defin.get(left, "")) & gloss_words(defin.get(right, ""))
        long_shared = any(len(word) >= 6 for word in shared)
        if on_eq and kun_l and kun_l == kun_r and (related or long_shared):
            return True
        # 缽/鉢: the same on-reading and gloss, with a kun reading on only one side.
        if on_eq and jac >= 0.5 and (not kun_l or not kun_r or kun_l == kun_r):
            return True
        # 賬/帳: one Lau headword, the same on-reading, and the jōyō form only
        # adds a kun reading. Identical kun readings are a different word.
        joy_l = left in joyo
        joy_r = right in joyo
        proper_kun = bool(kun_l and kun_r and (kun_l < kun_r or kun_r < kun_l))
        if (
            (lau[left] & lau[right])
            and on_eq
            and proper_kun
            and jac == 0.0
            and joy_l != joy_r
        ):
            return True
        return False

    def glyph_edges(cp: int) -> set[int]:
        out = set(jp_new.get(cp, ())) | set(zvar.get(cp, ()))
        for other in sem.get(cp, ()):
            if same_glyph(cp, other):
                out.add(other)
        out.discard(cp)
        return out

    def strict_edges(cp: int) -> set[int]:
        out = set(jp_new.get(cp, ())) | set(zvar.get(cp, ()))
        out.discard(cp)
        return out

    def walk(start: int, edges) -> set[int]:
        seen = {start}
        queue = [start]
        while queue and len(seen) < 24:
            node = queue.pop()
            for nxt in edges(node):
                if nxt not in seen:
                    seen.add(nxt)
                    queue.append(nxt)
        return seen

    def resolve(trad: int) -> int | None:
        """Jōyō shinjitai of this character, else the character when it is Japanese."""
        strict = walk(trad, strict_edges)
        strict_joy = {node for node in strict if is_jp(node) and node in joyo}
        if len(strict_joy) == 1:
            return next(iter(strict_joy))
        if len(strict_joy) > 1:
            return None
        full = walk(trad, glyph_edges)
        joy = {node for node in full if is_jp(node) and node in joyo}
        if len(joy) == 1:
            return next(iter(joy))
        if len(joy) > 1:
            return None
        jin = {node for node in full if is_jp(node) and node in jinmeiyo}
        if len(jin) == 1:
            return next(iter(jin))
        if len(jin) > 1:
            return trad if is_jp(trad) else None
        if is_jp(trad):
            return trad
        japanese = {node for node in full if is_jp(node)}
        if len(japanese) == 1:
            return next(iter(japanese))
        return None

    def seeds_of(cp: int) -> set[int]:
        seeds = set(trad_of.get(cp, ())) | set(simp_of.get(cp, ()))
        # Other traditional forms of the same simplified character (產 with 産).
        for simplified in simp_of.get(cp, ()):
            seeds |= trad_of.get(simplified, ())
        seeds.add(cp)
        seeds.discard(cp)
        return seeds

    mapping: dict[int, int] = {}
    ambiguous: set[int] = set()
    sources = {
        cp
        for cp in set(trad_of) | set(simp_of) | set(zvar)
        if is_cjk(cp) and not is_jp(cp)
    }
    for cp in sources:
        seeds = seeds_of(cp) | set(zvar.get(cp, ()))
        seeds.add(cp)
        targets: set[int] = set()
        for seed in seeds:
            resolved = resolve(seed)
            if resolved is not None and is_jp(resolved) and resolved != cp:
                targets.add(resolved)
        joys = {target for target in targets if target in joyo}
        if len(targets) > 1 and len(joys) == 1:
            joy = next(iter(joys))
            # Only when a listed traditional has this character as its shinjitai
            # (藥 → 薬). A second traditional that merely is jōyō (汇 → 彙/匯)
            # stays ambiguous.
            if any(joy in walk(seed, strict_edges) and joy != seed for seed in seeds):
                targets = {joy}
        if len(targets) == 1:
            mapping[cp] = next(iter(targets))
        elif len(targets) > 1:
            ambiguous.add(cp)
    return mapping, semantic_rows, ambiguous


def audit_gb0(mapping: dict[int, int], gb0: set[int], joyo: set[int], jinmeiyo: set[int]) -> None:
    """GB 2312 level 1 targets stay on a real Japanese character."""
    kyujitai_only = 0
    bad: list[str] = []
    for src in sorted(gb0):
        dst = mapping.get(src)
        if dst is None:
            continue
        if dst in joyo or dst in jinmeiyo:
            continue
        kyujitai_only += 1
        if not is_cjk(dst):
            bad.append(chr(src))
    if bad:
        raise SystemExit("kGB0 target is not a han character: " + " ".join(bad[:20]))
    print(f"kGB0 mappings that stay kyūjitai (no jōyō/jinmeiyō shinjitai): {kyujitai_only}")


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
        " * kTraditionalVariant, kZVariant). A jōyō or jinmeiyō form wins only\n"
        " * when it is the shinjitai of that same traditional character.\n"
        " * Little-endian (source, target) pairs. Sources are\n"
        " * not on the Japanese allowlist; each source has one allowlisted target.\n"
        " * Whitespace in the string is not part of the base64. */\n"
        "export const HAN_JP_PAIRS = `\n"
        + body
        + "\n`;\n"
    )
    RS_OUT.write_text(
        "//! Japanese Han substitutions.\n"
        "//!\n"
        f"//! Generated by `scripts/gen_han_jp_map.py` from Unicode {UNIHAN_VERSION} Unihan\n"
        "//! (`kSimplifiedVariant`, `kTraditionalVariant`, `kZVariant`). A jōyō or\n"
        "//! jinmeiyō form wins only when it is the shinjitai of that same\n"
        "//! traditional character.\n"
        "//! Little-endian `(source, target)` pairs. Sources are not on the Japanese\n"
        "//! allowlist; each source has one allowlisted target. Whitespace is ignored.\n"
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
    unihan = ensure_unihan()
    raw = load_variants(unihan)
    joyo, jinmeiyo, gb0 = load_sets(unihan)
    on, kun, defin, matt, lau = load_word_data(unihan)
    mapping, semantic_rows, ambiguous = build_map(
        raw, is_jp, joyo, jinmeiyo, on, kun, defin, matt, lau
    )
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
    expect(mapping, "线", "線")
    expect(mapping, "举", "挙")
    expect(mapping, "鉴", "鑑")
    expect(mapping, "净", "浄")
    expect(mapping, "账", "帳")
    expect(mapping, "谭", "譚")
    expect(mapping, "岗", "崗")
    expect(mapping, "箫", "簫")
    expect(mapping, "鲶", "鯰")
    if mapping.get(ord("岗")) == ord("岡"):
        raise SystemExit("岗 must not become 岡")
    if mapping.get(ord("箫")) == ord("焼"):
        raise SystemExit("箫 must not become 焼")
    if mapping.get(ord("鲶")) == ord("鮎"):
        raise SystemExit("鲶 must not become 鮎")
    audit_gb0(mapping, gb0, joyo, jinmeiyo)
    print(f"ambiguous sources left unmapped: {len(ambiguous)}")
    for ch in "国学体来与个无从广干后面里会":
        if ord(ch) in mapping:
            raise SystemExit(f"{ch} is Japanese and must not be rewritten")
    encoded = pack(mapping)
    write_outputs(encoded)
    print(f"mappings: {len(mapping)}")
    print(f"semantic variant rows read: {semantic_rows}")
    print(f"wrote {TS_OUT.relative_to(ROOT)} and {RS_OUT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
