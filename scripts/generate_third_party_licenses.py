#!/usr/bin/env python3
"""Generate THIRD_PARTY_LICENSES for Rust and JavaScript dependencies.

Re-run from the repository root before a release:

    python3 scripts/generate_third_party_licenses.py

Rust: `cargo metadata` for the resolved graph (the same information
cargo-about reads), plus LICENSE files from the local cargo registry after
`cargo fetch`. JavaScript: `license-checker --production` when it runs,
otherwise the production packages already installed under node_modules
(the same inputs license-checker uses).

The Windows x64 graph is the one the installer ships. Pass --platform to
override it.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "THIRD_PARTY_LICENSES"
MANIFEST = ROOT / "src-tauri" / "Cargo.toml"
DEFAULT_PLATFORM = "x86_64-pc-windows-msvc"
def is_license_filename(name: str) -> bool:
    lower = name.lower().replace("_", "-")
    if lower.startswith(".") or lower.endswith((".png", ".jpg", ".svg", ".woff", ".woff2")):
        return False
    stems = ("license", "licence", "copying", "notice", "unlicense", "copyright")
    return any(lower == stem or lower.startswith(stem + ".") or lower.startswith(stem + "-") for stem in stems)
COPYLEFT_MARKERS = (
    "GPL",
    "AGPL",
    "LGPL",
    "EUPL",
    "OSL",
    "CPAL",
    "SSPL",
    "RPL",
    "CECILL",
)


def cargo_bin() -> list[str]:
    """Prefer `cargo +stable` when the default toolchain cannot parse the lockfile."""
    for prefix in (["cargo", "+stable"], ["cargo"]):
        proc = subprocess.run(prefix + ["--version"], capture_output=True, text=True)
        if proc.returncode == 0:
            return prefix
    return ["cargo"]


def run_metadata(platform: str) -> dict:
    cmd = cargo_bin() + [
        "metadata",
        "--locked",
        "--format-version",
        "1",
        "--manifest-path",
        str(MANIFEST),
        "--filter-platform",
        platform,
    ]
    proc = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr)
        raise SystemExit(f"cargo metadata failed ({proc.returncode})")
    return json.loads(proc.stdout)


def runtime_package_ids(meta: dict) -> set[str]:
    nodes = {node["id"]: node for node in meta["resolve"]["nodes"]}
    root = meta["resolve"]["root"]
    seen: set[str] = set()
    stack = [root]
    while stack:
        current = stack.pop()
        if current in seen or current not in nodes:
            continue
        seen.add(current)
        for dep in nodes[current].get("deps", []):
            kinds = [item.get("kind") for item in dep.get("dep_kinds", [])]
            if not kinds or any(kind in (None, "normal") for kind in kinds):
                stack.append(dep["pkg"])
    seen.discard(root)
    return seen


def registry_roots() -> list[Path]:
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    src = cargo_home / "registry" / "src"
    if not src.is_dir():
        return []
    return [path for path in src.iterdir() if path.is_dir()]


def license_files_for(name: str, version: str) -> list[Path]:
    folder = f"{name}-{version}"
    found: list[Path] = []
    for root in registry_roots():
        crate = root / folder
        if not crate.is_dir():
            continue
        for path in crate.iterdir():
            if path.is_file() and is_license_filename(path.name):
                found.append(path)
        break
    return sorted(found)


def read_text(path: Path) -> str:
    data = path.read_bytes()
    for encoding in ("utf-8", "latin-1"):
        try:
            return data.decode(encoding).replace("\r\n", "\n").strip()
        except UnicodeDecodeError:
            continue
    return data.decode("utf-8", "replace").strip()


def mpl_obligated(expression: str) -> bool:
    """True when MPL-2.0 is not paired with a permissive alternative."""
    normalized = expression.upper().replace(" / ", " OR ").replace("/", " OR ")
    if "MPL-2.0" not in normalized:
        return False
    permissive = any(
        token in normalized
        for token in ("MIT", "APACHE", "BSD", "ISC", "ZLIB", "UNLICENSE", "CC0", "0BSD")
    )
    if permissive and " OR " in normalized:
        return False
    return True


def is_copyleft(expression: str) -> bool:
    upper = expression.upper()
    if "MPL" in upper and "GPL" not in upper and "LGPL" not in upper:
        return False
    return any(marker in upper for marker in COPYLEFT_MARKERS)


def js_packages() -> list[dict]:
    checker = shutil.which("license-checker")
    commands = []
    if checker:
        commands.append([checker, "--production", "--json", "--start", str(ROOT)])
    npx = shutil.which("npx")
    if npx:
        commands.append(
            [npx, "--yes", "license-checker", "--production", "--json", "--start", str(ROOT)]
        )
    for cmd in commands:
        proc = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True)
        if proc.returncode == 0 and proc.stdout.strip().startswith("{"):
            return packages_from_license_checker(json.loads(proc.stdout))
    return packages_from_node_modules()


def packages_from_license_checker(payload: dict) -> list[dict]:
    rows = []
    for key, info in payload.items():
        name, _, version = key.rpartition("@")
        if not name:
            name, version = key, info.get("version") or ""
        license_file = info.get("licenseFile") or ""
        text = ""
        if license_file and Path(license_file).is_file():
            text = read_text(Path(license_file))
        rows.append(
            {
                "name": name,
                "version": version or str(info.get("version") or ""),
                "license": str(info.get("licenses") or "unknown"),
                "repository": str(info.get("repository") or ""),
                "text": text,
            }
        )
    rows.sort(key=lambda row: (row["name"].lower(), row["version"]))
    return rows


def packages_from_node_modules() -> list[dict]:
    root = ROOT / "node_modules"
    rows = []
    if not root.is_dir():
        return rows
    for manifest in root.rglob("package.json"):
        if "node_modules" in manifest.relative_to(root).parts[:-1]:
            # rglob already descends into nested node_modules; skip nothing extra
            pass
        try:
            data = json.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        name = data.get("name")
        if not isinstance(name, str) or not name or name.startswith("."):
            continue
        license_field = data.get("license") or data.get("licenses") or "unknown"
        if isinstance(license_field, dict):
            license_field = license_field.get("type") or "unknown"
        if isinstance(license_field, list):
            license_field = " OR ".join(str(item) for item in license_field)
        texts = []
        for path in manifest.parent.iterdir():
            if path.is_file() and is_license_filename(path.name):
                texts.append(read_text(path))
        repo = data.get("repository") or ""
        if isinstance(repo, dict):
            repo = repo.get("url") or ""
        rows.append(
            {
                "name": name,
                "version": str(data.get("version") or ""),
                "license": str(license_field),
                "repository": str(repo),
                "text": "\n\n".join(texts),
            }
        )
    rows.sort(key=lambda row: (row["name"].lower(), row["version"]))
    # The walk visits nested copies. Keep the first of each name@version.
    unique = []
    seen = set()
    for row in rows:
        key = (row["name"], row["version"])
        if key in seen:
            continue
        seen.add(key)
        unique.append(row)
    return unique


def crate_rows(meta: dict, ids: set[str]) -> list[dict]:
    by_id = {pkg["id"]: pkg for pkg in meta["packages"]}
    rows = []
    for ident in ids:
        pkg = by_id.get(ident)
        if not pkg:
            continue
        name = pkg["name"]
        version = pkg["version"]
        files = license_files_for(name, version)
        text = "\n\n".join(f"----- {path.name} -----\n{read_text(path)}" for path in files)
        rows.append(
            {
                "name": name,
                "version": version,
                "license": pkg.get("license") or "unspecified",
                "repository": pkg.get("repository") or "",
                "text": text,
            }
        )
    rows.sort(key=lambda row: (row["name"].lower(), row["version"]))
    return rows


def source_lines(row: dict) -> list[str]:
    lines = [f"- {row['name']} {row['version']}"]
    if row["repository"]:
        lines.append(f"  {row['repository']}")
    lines.append(f"  https://crates.io/crates/{row['name']}/{row['version']}")
    return lines


def grouped_sections(rows: list[dict], ecosystem: str) -> list[str]:
    # Group by identical license text so the file is not one copy per crate.
    by_hash: dict[str, list[dict]] = defaultdict(list)
    hash_order: list[str] = []
    unlabeled: list[dict] = []
    for row in rows:
        if not row["text"]:
            unlabeled.append(row)
            continue
        digest = hashlib.sha256(row["text"].encode("utf-8")).hexdigest()
        if digest not in by_hash:
            hash_order.append(digest)
        by_hash[digest].append(row)
    parts: list[str] = []
    by_spdx: dict[str, list[dict]] = defaultdict(list)
    for row in unlabeled:
        by_spdx[row["license"]].append(row)
    if by_spdx:
        parts.append(f"## {ecosystem} packages without a license file in the local cache\n")
        for expression in sorted(by_spdx):
            parts.append(f"### {expression}\n")
            for row in by_spdx[expression]:
                parts.extend(source_lines(row) if ecosystem == "Rust" else js_source_lines(row))
            parts.append("")
    for digest in hash_order:
        group = by_hash[digest]
        expressions = sorted({row["license"] for row in group})
        parts.append(f"## {ecosystem} license text ({'; '.join(expressions)})\n")
        parts.append("Packages:\n")
        for row in group:
            if ecosystem == "Rust":
                parts.extend(source_lines(row))
            else:
                parts.extend(js_source_lines(row))
        parts.append("")
        parts.append(group[0]["text"])
        parts.append("")
    return parts


def js_source_lines(row: dict) -> list[str]:
    lines = [f"- {row['name']} {row['version']} ({row['license']})"]
    if row["repository"]:
        lines.append(f"  {row['repository']}")
    return lines


def render(platform: str, rust: list[dict], javascript: list[dict]) -> str:
    mpl = [row for row in rust if mpl_obligated(row["license"])]
    js_copyleft = [row for row in javascript if is_copyleft(row["license"])]
    rust_copyleft = [row for row in rust if is_copyleft(row["license"])]
    lines = [
        "THIRD-PARTY SOFTWARE LICENSES",
        "Argos",
        "",
        "Generated by scripts/generate_third_party_licenses.py.",
        "Re-run that script from the repository root before a release:",
        "",
        "    python3 scripts/generate_third_party_licenses.py",
        "",
        "Argos itself is Apache License, Version 2.0. See LICENSE and NOTICE.",
        "NOTICE also carries the IPADIC notice (including its no-warranty text),",
        "the Unicode License v3 notice for src-tauri/src/llm/japanese_han_map.rs",
        "(derived from Unihan 18.0), and the e-Gov / 公共データ利用規約 (PDL1.0)",
        "source and edit statement for resources/statutes.db.gz.",
        "",
        f"Rust packages are the normal (runtime) dependencies of src-tauri for {platform}.",
        "JavaScript packages are the production dependencies of the UI.",
        "",
        "## MPL-2.0 components (used unmodified)",
        "",
        "These MPL-2.0 components are used unmodified. Argos does not modify their",
        "source. Source for the resolved version is available at the repository",
        "and at crates.io. A crate that offers MPL-2.0 only as an alternative to a",
        "permissive license is used under the permissive license and is not listed here.",
        "",
    ]
    if mpl:
        for row in mpl:
            lines.extend(source_lines(row))
            lines.append(f"  license: {row['license']}")
    else:
        lines.append("(none found in this dependency graph)")
    lines.append("")
    if rust_copyleft or js_copyleft:
        lines.append("## Other copyleft licenses")
        lines.append("")
        for row in rust_copyleft + js_copyleft:
            lines.append(f"- {row['name']} {row['version']} ({row['license']})")
        lines.append("")
    else:
        lines.append("No GPL, AGPL, or LGPL packages were found in this graph.")
        lines.append("JavaScript production dependencies include no copyleft license.")
        lines.append("")
    lines.append("# Rust")
    lines.append("")
    lines.extend(grouped_sections(rust, "Rust"))
    lines.append("# JavaScript")
    lines.append("")
    if javascript:
        lines.extend(grouped_sections(javascript, "JavaScript"))
    else:
        lines.append("No production JavaScript packages were found. Install them and re-run.")
        lines.append("")
    text = "\n".join(lines).rstrip() + "\n"
    return text


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--platform", default=DEFAULT_PLATFORM)
    args = parser.parse_args()
    meta = run_metadata(args.platform)
    rust = crate_rows(meta, runtime_package_ids(meta))
    javascript = js_packages()
    OUT.write_text(render(args.platform, rust, javascript), encoding="utf-8")
    mpl = [row["name"] for row in rust if mpl_obligated(row["license"])]
    choice = [
        row["name"]
        for row in rust
        if "MPL-2.0" in row["license"].upper() and row["name"] not in mpl
    ]
    print(f"wrote {OUT} ({OUT.stat().st_size} bytes)")
    print(f"rust packages: {len(rust)}")
    print(f"javascript packages: {len(javascript)}")
    print("MPL-2.0 only: " + ", ".join(mpl))
    print("MPL-2.0 as a permissive alternative: " + ", ".join(choice))


if __name__ == "__main__":
    main()
