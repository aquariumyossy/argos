#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Extract Argos search-word CSV terms from major civil statutes via e-Gov Law API v2.

Usage:
  python scripts/extract-legal-terms.py
  python scripts/extract-legal-terms.py --out testdata/search-words-legal-civil.csv

Requires: Python 3.10+ (stdlib only; uses urllib).

Outputs CSV rows as: 表層,品詞,読み  (reading left empty in v1)
Compatible with Argos Settings → 検索ワード登録 → CSV 取り込み.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import OrderedDict
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_LAWS = Path(__file__).resolve().parent / "legal-terms-laws.json"
DEFAULT_OUT = ROOT / "testdata" / "search-words-legal-civil.csv"
CACHE_DIR = Path(__file__).resolve().parent / ".cache" / "laws"
API_BASE = "https://laws.e-gov.go.jp/api/2"
USER_AGENT = "argos-legal-terms/1.0 (+https://github.com/aquariumyossy/argos)"

# Structural titles: "第一章　総則" / "第一節　意思表示"
STRUCT_TITLE_RE = re.compile(
    r"^第[一二三四五六七八九十百千0-9０-９]+"
    r"(?:編|章|節|款|目|条の?\d*|"
    r"[のノ][一二三四五六七八九十百千0-9０-９]+)*"
    r"[　\s]+(.+)$"
)

# Article / paragraph captions: （時効の援用）
CAPTION_RE = re.compile(r"^[（(]\s*(.+?)\s*[）)]$")

# Definition quotes: 「個人情報」とは
DEF_QUOTE_RE = re.compile(r"「([^」]{2,40})」とは")

# Particle compounds useful for phrase search (within short captions / defs)
PARTICLE_COMPOUND_RE = re.compile(
    r"(?:[^、。；;\s　]{1,20})"
    r"(?:による|に基づく|についての|に対する|のための)"
    r"(?:[^、。；;\s　]{1,20})"
)

SKIP_EXACT = {
    "総則",
    "通則",
    "雑則",
    "罰則",
    "附則",
    "削除",
    "趣旨",
    "目的",
    "定義",
    "適用",
    "施行",
    "経過措置",
    "方法",
    "要件",
    "手続",
    "手続き",
    "内容",
    "意義",
    "効果",
    "範囲",
    "制限",
    "定め",
    "場合",
    "特例",
    "基準",
    "条件",
    "事由",
    "原則",
    "例外",
}

SKIP_CONTAINS = (
    "この法律",
    "この章",
    "この節",
    "次条",
    "前項",
    "次項",
    "前条",
    "前二項",
    "前各項",
    "規定による",
    "に関する経過措置",
    "についての経過措置",
)

# Article captions often end with these meta nouns (「○○の方法」「○○の要件」).
# They create many low-value dictionary variants for search.
WEAK_NO_SUFFIXES = (
    "の方法",
    "の要件",
    "の手続",
    "の手続き",
    "の内容",
    "の意義",
    "の効果",
    "の範囲",
    "の制限",
    "の定め",
    "の場合",
    "の特例",
    "の適用",
    "の届出",
    "の申出",
    "の申請",
    "の規定",
    "の基準",
    "の条件",
    "の事由",
    "の原則",
    "の例外",
    "の目的",
    "の趣旨",
    "の定義",
    "の解釈",
    "の権限",
    "の義務",
    "の責任",
    "の承認",
    "の許可",
    "の同意",
    "の承諾",
    "の通知",
    "の催告",
    "の告知",
    "の公示",
    "の公告",
    "の登録",
    "の変更",
    "の廃止",
    "の停止",
    "の中止",
    "の終了",
    "の開始",
    "の成立",
    "の発生",
    "の消滅",
    "の移転",
    "の取得",
    "の喪失",
    "の提出",
    "の交付",
    "の備置",
    "の閲覧",
    "の謄本",
    "の抄本",
    "の選任",
    "の解任",
    "の辞任",
    "の就任",
    "の任期",
    "の報酬",
    "の費用",
    "の利息",
    "の損害",
    "の賠償",
    "の補償",
    "の弁済",  # keep 弁済による代位 via particle pattern; bare 「Xの弁済」 is weak
    "の供託",
    "の支払",
    "の支払い",
    "の返還",
    "の引渡し",
    "の引渡",
    "の保管",
    "の管理",
    "の利用",
    "の使用",
    "の収益",
    "の処分",
    "の分割",
    "の合併",
    "の計算",
    "の勘定",
    "の報告",
    "の開示",
    "の公表",
    "の公表等",
)


def node_text(node) -> str:
    if isinstance(node, str):
        return node
    if isinstance(node, dict):
        return "".join(node_text(c) for c in node.get("children") or [])
    return ""


def walk(node, visitor):
    if not isinstance(node, dict):
        return
    visitor(node)
    for child in node.get("children") or []:
        walk(child, visitor)


def fetch_law(law_id: str, *, use_cache: bool = True) -> dict:
    CACHE_DIR.mkdir(parents=True, exist_ok=True)
    cache_path = CACHE_DIR / f"{law_id}.json"
    if use_cache and cache_path.exists():
        return json.loads(cache_path.read_text(encoding="utf-8"))

    url = f"{API_BASE}/law_data/{urllib.parse.quote(law_id)}?response_format=json"
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=180) as resp:
            data = json.load(resp)
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"e-Gov API error for {law_id}: HTTP {e.code}") from e
    cache_path.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
    return data


def normalize_term(raw: str) -> str | None:
    t = raw.strip()
    t = t.replace("\u3000", " ").strip()
    t = re.sub(r"\s+", "", t)  # compound search terms usually have no spaces
    # strip lingering brackets
    t = t.strip("（）()「」『』【】[]")
    if not t:
        return None
    # Keep short search phrases only (8+ chars are rarely useful as dict chips).
    if len(t) < 2 or len(t) >= 8:
        return None
    if t in SKIP_EXACT:
        return None
    if any(s in t for s in SKIP_CONTAINS):
        return None
    if re.fullmatch(r"[0-9０-９一二三四五六七八九十百千]+", t):
        return None
    if re.fullmatch(r"第[一二三四五六七八九十百千0-9０-９のノ条項号編章節款目]+", t):
        return None
    # Avoid pure procedural boilerplate
    if t.endswith("場合") and len(t) <= 4:
        return None
    # Drop formulaic 「○○の方法／要件／手続…」 variants.
    if any(t.endswith(suf) for suf in WEAK_NO_SUFFIXES):
        return None
    return t


def clean_struct_title(text: str) -> str | None:
    text = text.strip()
    m = STRUCT_TITLE_RE.match(text)
    if m:
        return normalize_term(m.group(1))
    return normalize_term(text)


def clean_caption(text: str) -> str | None:
    text = text.strip()
    m = CAPTION_RE.match(text)
    if m:
        return normalize_term(m.group(1))
    return normalize_term(text)


def extract_from_law(data: dict, label: str) -> OrderedDict[str, str]:
    """Return ordered map term -> pos label (law short name)."""
    found: OrderedDict[str, str] = OrderedDict()

    def add(term: str | None, source: str):
        if not term:
            return
        if term not in found:
            found[term] = label
        # also harvest particle compounds inside the term / caption
        if source in ("caption", "struct", "definition"):
            for m in PARTICLE_COMPOUND_RE.finditer(term):
                sub = normalize_term(m.group(0))
                if sub and sub not in found:
                    found[sub] = label

    root = data.get("law_full_text")
    if not root:
        return found

    caption_tags = {
        "ArticleCaption",
        "ParagraphCaption",
    }
    struct_tags = {
        "PartTitle",
        "ChapterTitle",
        "SectionTitle",
        "SubsectionTitle",
        "DivisionTitle",
    }
    sentence_tags = {
        "Sentence",
        "ParagraphSentence",
        "ItemSentence",
        "Subitem1Sentence",
    }

    def visitor(node: dict):
        tag = node.get("tag")
        text = node_text(node).strip()
        if not text:
            return
        if tag in caption_tags:
            add(clean_caption(text), "caption")
        elif tag in struct_tags:
            add(clean_struct_title(text), "struct")
        elif tag in sentence_tags:
            for m in DEF_QUOTE_RE.finditer(text):
                add(normalize_term(m.group(1)), "definition")
            # Do not harvest free particle compounds from full sentences —
            # that pulls in long proviso fragments. Captions/defs are enough.

    walk(root, visitor)
    return found


def write_csv(path: Path, terms: OrderedDict[str, str], law_labels: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    now = datetime.now(timezone.utc).astimezone().strftime("%Y-%m-%d %H:%M:%S %z")
    lines = [
        "# Argos 検索ワード（主要民事法令・e-Gov 自動抽出）",
        "# 形式: 表層,品詞,読み",
        f"# 生成: {now}",
        f"# 対象: {' / '.join(law_labels)}",
        "# 再生成: python scripts/extract-legal-terms.py",
        f"# 件数: {len(terms)}",
    ]
    buf = []
    for term, label in terms.items():
        # escape CSV manually for simplicity
        def esc(s: str) -> str:
            if any(c in s for c in ',"\n\r'):
                return '"' + s.replace('"', '""') + '"'
            return s

        buf.append(f"{esc(term)},{esc(label)},")

    path.write_text("\n".join(lines + buf) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--laws", type=Path, default=DEFAULT_LAWS)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--no-cache", action="store_true")
    parser.add_argument("--sleep", type=float, default=0.3, help="Delay between API calls")
    args = parser.parse_args(argv)

    config = json.loads(args.laws.read_text(encoding="utf-8"))
    laws = config.get("laws") or []
    if not laws:
        print("No laws in config", file=sys.stderr)
        return 1

    merged: OrderedDict[str, str] = OrderedDict()
    labels: list[str] = []

    for i, law in enumerate(laws):
        law_id = law["id"]
        label = law.get("label") or law.get("title") or law_id
        labels.append(label)
        print(f"[{i + 1}/{len(laws)}] fetching {label} ({law_id})...", flush=True)
        data = fetch_law(law_id, use_cache=not args.no_cache)
        api_title = (data.get("revision_info") or {}).get("law_title")
        if api_title:
            print(f"  title={api_title}", flush=True)
        part = extract_from_law(data, label)
        print(f"  extracted={len(part)}", flush=True)
        for term, lab in part.items():
            if term not in merged:
                merged[term] = lab
        if i + 1 < len(laws):
            time.sleep(args.sleep)

    write_csv(args.out, merged, labels)
    print(f"Wrote {len(merged)} terms -> {args.out}", flush=True)

    # Smoke hints
    samples = ["時効の援用", "弁済による代位", "個人情報", "消費者契約"]
    for s in samples:
        print(f"  contains[{s}]={s in merged}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
