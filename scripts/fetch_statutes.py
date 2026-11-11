#!/usr/bin/env python3
"""Rebuild src-tauri/resources/statutes.db.gz from the e-Gov law API (no key).

Uses法令API Version 1:
  https://laws.e-gov.go.jp/api/1/lawlists/2
  https://laws.e-gov.go.jp/api/1/lawdata/{lawId}

The article split matches src-tauri/src/statute/xmlparse.rs:
skip the table of contents, deleted articles, and articles inside AmendProvision.
"""

from __future__ import annotations

import gzip
import sqlite3
import sys
import time
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path

ORIGIN = "https://laws.e-gov.go.jp/api/1"
UA = "Argos/1.18 (statute bundle; e-Gov law API v1)"
ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "src-tauri" / "src" / "statute" / "catalog.rs"
OUT = ROOT / "src-tauri" / "resources" / "statutes.db.gz"
BUNDLE_VERSION = "2026-11-11b"

SCHEMA = """
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE law (
  law_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  law_num TEXT NOT NULL DEFAULT '',
  as_of TEXT NOT NULL DEFAULT '',
  checked_on TEXT NOT NULL DEFAULT '',
  source TEXT NOT NULL DEFAULT 'bundle'
);
CREATE TABLE law_alias (alias TEXT PRIMARY KEY, law_id TEXT NOT NULL);
CREATE TABLE law_name (
  law_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  law_num TEXT NOT NULL DEFAULT ''
);
CREATE TABLE article (
  law_id TEXT NOT NULL,
  article_key TEXT NOT NULL,
  article_num TEXT NOT NULL,
  heading TEXT NOT NULL DEFAULT '',
  body TEXT NOT NULL,
  as_of TEXT NOT NULL DEFAULT '',
  probed_on TEXT NOT NULL DEFAULT '',
  suppl INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (law_id, article_key)
);
CREATE TABLE law_update (law_id TEXT PRIMARY KEY, as_of TEXT NOT NULL);
CREATE TABLE update_day (day TEXT PRIMARY KEY);
CREATE INDEX idx_law_alias_id ON law_alias(law_id);
CREATE INDEX idx_law_name_name ON law_name(name);
"""


def local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def squash(text: str) -> str:
    return "".join(text.split())


def text_of(el: ET.Element, skip_rt: bool = True) -> str:
    parts: list[str] = []

    def walk(node: ET.Element) -> None:
        if skip_rt and local(node.tag) == "Rt":
            return
        if node.text:
            parts.append(node.text)
        for child in list(node):
            walk(child)
            if child.tail:
                parts.append(child.tail)

    if el.text:
        parts.append(el.text)
    for child in list(el):
        walk(child)
        if child.tail:
            parts.append(child.tail)
    return "".join(parts)


def article_identity(num: str, suppl: bool) -> tuple[str, str] | None:
    num = num.strip()
    if not num:
        return None
    parts: list[int] = []
    for part in num.split("_"):
        if not part.isdigit():
            return None
        value = int(part)
        if value <= 0 or value > 999_999:
            return None
        parts.append(value)
    if not parts:
        return None
    key_body = "_".join(str(value) for value in parts)
    key = f"s{key_body}" if suppl else key_body
    display = ("附則" if suppl else "") + f"第{parts[0]}条"
    for branch in parts[1:]:
        display += f"の{branch}"
    return key, display


def display_law_name(law_num: str, law_name: str) -> str:
    name = squash(law_name)
    num = squash(law_num)
    if not num or not name.startswith(num):
        return name
    rest = name[len(num) :].strip().strip("（()）").strip()
    return rest or name


def cleanup_heading(raw: str) -> str:
    text = squash(raw)
    return text.strip("（）() ")


def cleanup_body(raw: str) -> str:
    lines = []
    for line in raw.splitlines():
        line = line.strip()
        if line:
            lines.append(line)
    return "\n".join(lines).strip()


def parse_law(xml: str) -> dict:
    root = ET.fromstring(xml)
    law_id = ""
    law_num = ""
    name = ""
    abbrev = ""
    articles: list[dict] = []

    def walk(node: ET.Element, amend: int, toc: int, suppl: int) -> None:
        nonlocal law_id, law_num, name, abbrev
        name_l = local(node.tag)
        if name_l == "LawId" and not law_id:
            law_id = squash(text_of(node, skip_rt=False))
        elif name_l == "LawNum" and not law_num:
            law_num = squash(text_of(node, skip_rt=False))
        elif name_l == "LawTitle" and not name:
            name = display_law_name(law_num, text_of(node))
            if not abbrev:
                abbrev = (node.attrib.get("Abbrev") or "").strip()
        elif name_l == "Article" and amend == 0 and toc == 0:
            ident = article_identity(node.attrib.get("Num", ""), suppl > 0)
            deleted = (node.attrib.get("Delete") or "").lower() == "true"
            if ident and not deleted:
                key, display = ident
                heading = ""
                chunks: list[str] = []
                for cap in node.iter():
                    if local(cap.tag) == "ArticleCaption":
                        heading = cleanup_heading(text_of(cap))
                        break
                body = render_article(node)
                body = cleanup_body(body)
                if body:
                    articles.append(
                        {
                            "key": key,
                            "num": display,
                            "heading": heading,
                            "body": body,
                            "suppl": 1 if suppl else 0,
                        }
                    )
            return
        child_amend = amend + (1 if name_l == "AmendProvision" else 0)
        child_toc = toc + (1 if name_l == "TOC" else 0)
        child_suppl = suppl + (1 if name_l == "SupplProvision" else 0)
        for child in list(node):
            walk(child, child_amend, child_toc, child_suppl)

    walk(root, 0, 0, 0)
    seen: dict[str, int] = {}
    for article in articles:
        seen[article["key"]] = seen.get(article["key"], 0) + 1
        if seen[article["key"]] > 1:
            article["key"] = f"{article['key']}#{seen[article['key']]}"
    return {
        "law_id": law_id,
        "law_num": law_num,
        "name": name,
        "abbrev": abbrev,
        "articles": articles,
    }


BLOCK = {
    "Paragraph",
    "Item",
    "Subitem1",
    "Subitem2",
    "Subitem3",
    "Subitem4",
    "Subitem5",
    "Subitem6",
    "Subitem7",
    "Subitem8",
    "Subitem9",
    "Subitem10",
}
LABELS = {"ItemTitle", "Subitem1Title", "Subitem2Title", "Subitem3Title"}


def render_article(article: ET.Element) -> str:
    parts: list[str] = []

    def append_text(node: ET.Element, after_label: bool = False) -> None:
        if local(node.tag) == "Rt":
            return
        if node.text and node.text.strip():
            parts.append(node.text.strip())
            if after_label:
                parts.append("　")
                after_label = False
        for child in list(node):
            kind = local(child.tag)
            if kind in {"ArticleCaption", "ArticleTitle"}:
                if child.tail and child.tail.strip():
                    parts.append(child.tail.strip())
                continue
            if kind in BLOCK and parts and not parts[-1].endswith("\n"):
                parts.append("\n")
            append_text(child, after_label=kind in LABELS)
            if child.tail and child.tail.strip():
                parts.append(child.tail.strip())

    append_text(article)
    return "".join(parts)


def fetch(url: str) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=120) as resp:
        return resp.read().decode("utf-8")


def load_catalog() -> list[tuple[str, bool, list[str]]]:
    text = CATALOG.read_text(encoding="utf-8")
    entries: list[tuple[str, bool, list[str]]] = []
    # entry("ID", true, &["a", "b"])
    import re

    for match in re.finditer(
        r'entry\(\s*"([^"]+)"\s*,\s*(true|false)\s*,\s*&\[(.*?)\]\s*,?\s*\)',
        text,
        re.S,
    ):
        law_id, flag, alias_src = match.group(1), match.group(2) == "true", match.group(3)
        aliases = re.findall(r'"([^"]+)"', alias_src)
        entries.append((law_id, flag, aliases))
    if len(entries) < 40:
        raise SystemExit(f"catalog parse failed: {len(entries)} entries")
    return entries


def main() -> None:
    today = time.strftime("%Y-%m-%d")
    dest = Path(sys.argv[1]) if len(sys.argv) > 1 else OUT
    db_path = dest.with_suffix(".build.db")
    if db_path.exists():
        db_path.unlink()
    conn = sqlite3.connect(db_path)
    conn.executescript(SCHEMA)
    print("law list", flush=True)
    listing = fetch(f"{ORIGIN}/lawlists/2")
    root = ET.fromstring(listing)
    names = []
    for node in root.iter():
        if local(node.tag) != "LawNameListInfo":
            continue
        law_id = name = law_num = ""
        for child in list(node):
            kind = local(child.tag)
            if kind == "LawId":
                law_id = squash(text_of(child, skip_rt=False))
            elif kind == "LawName":
                name = squash(text_of(child, skip_rt=False))
            elif kind == "LawNo":
                law_num = squash(text_of(child, skip_rt=False))
        if law_id and name:
            names.append((law_id, name, law_num))
    conn.executemany(
        "INSERT OR REPLACE INTO law_name (law_id, name, law_num) VALUES (?, ?, ?)",
        names,
    )
    print(f"names {len(names)}", flush=True)
    catalog = load_catalog()
    for law_id, bundle, aliases in catalog:
        if not bundle:
            name = aliases[0] if aliases else law_id
            conn.execute(
                "INSERT OR REPLACE INTO law (law_id, name, law_num, as_of, checked_on, source) VALUES (?, ?, '', '', '', 'alias')",
                (law_id, name),
            )
            for alias in aliases:
                conn.execute(
                    "INSERT OR REPLACE INTO law_alias (alias, law_id) VALUES (?, ?)",
                    (alias, law_id),
                )
            continue
        print(f"fetch {law_id}", flush=True)
        time.sleep(0.35)
        parsed = parse_law(fetch(f"{ORIGIN}/lawdata/{law_id}"))
        if not parsed["articles"]:
            raise SystemExit(f"{law_id}: e-Gov response had no articles")
        use_id = parsed["law_id"] or law_id
        name = parsed["name"] or (aliases[0] if aliases else use_id)
        all_aliases = list(dict.fromkeys([*aliases, parsed["abbrev"], parsed["name"], name]))
        all_aliases = [a for a in all_aliases if a and len(a) >= 2]
        conn.execute("DELETE FROM article WHERE law_id = ?", (use_id,))
        conn.execute("DELETE FROM law_alias WHERE law_id = ?", (use_id,))
        conn.execute(
            "INSERT OR REPLACE INTO law (law_id, name, law_num, as_of, checked_on, source) VALUES (?, ?, ?, ?, ?, 'bundle')",
            (use_id, name, parsed["law_num"], today, today),
        )
        conn.executemany(
            "INSERT OR REPLACE INTO law_alias (alias, law_id) VALUES (?, ?)",
            [(alias, use_id) for alias in all_aliases],
        )
        conn.executemany(
            "INSERT INTO article (law_id, article_key, article_num, heading, body, as_of, probed_on, suppl) VALUES (?, ?, ?, ?, ?, ?, '', ?)",
            [
                (use_id, a["key"], a["num"], a["heading"], a["body"], today, a["suppl"])
                for a in parsed["articles"]
            ],
        )
        conn.commit()
        print(f"  {name} articles={len(parsed['articles'])}", flush=True)
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('bundle_version', ?)",
        (BUNDLE_VERSION,),
    )
    laws = conn.execute("SELECT COUNT(*) FROM law WHERE source = 'bundle'").fetchone()[0]
    articles = conn.execute("SELECT COUNT(*) FROM article").fetchone()[0]
    conn.commit()
    conn.close()
    dest.parent.mkdir(parents=True, exist_ok=True)
    with open(db_path, "rb") as src, gzip.open(dest, "wb", compresslevel=9) as out:
        out.write(src.read())
    db_path.unlink()
    print(f"laws={laws} articles={articles} gzip={dest.stat().st_size} -> {dest}")


if __name__ == "__main__":
    main()
