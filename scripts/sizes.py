#!/usr/bin/env python3
"""Generate the README binary-sizes table from cities/*.jsonc + transit-viz/public/data/*.bin."""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CITIES_DIR = ROOT / "cities"
DATA_DIR = ROOT / "transit-viz/public/data"
README = ROOT / "README.md"
BEGIN = "<!-- BEGIN sizes -->"
END = "<!-- END sizes -->"


def strip_jsonc_comments(text: str) -> str:
    out = []
    i, n = 0, len(text)
    in_str = False
    while i < n:
        c = text[i]
        if in_str:
            out.append(c)
            if c == "\\" and i + 1 < n:
                out.append(text[i + 1])
                i += 2
                continue
            if c == '"':
                in_str = False
            i += 1
            continue
        if c == '"':
            in_str = True
            out.append(c)
            i += 1
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i)
            i = n if j == -1 else j
            continue
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            j = text.find("*/", i + 2)
            i = n if j == -1 else j + 2
            continue
        out.append(c)
        i += 1
    return "".join(out)


def load_jsonc(p: Path) -> dict:
    return json.loads(strip_jsonc_comments(p.read_text()))


def human(n: int) -> str:
    if n < 1024:
        return f"{n}B"
    for unit in ("K", "M", "G"):
        v = n / 1024 ** ("KMG".index(unit) + 1)
        if v < 1024 or unit == "G":
            s = f"{v:.1f}"
            return f"{s}{unit}"
    return f"{n}B"


def build_table() -> str:
    rows = []
    for cfg in sorted(CITIES_DIR.glob("*.jsonc")):
        data = load_jsonc(cfg)
        bin_path = DATA_DIR / data["file"]
        if not bin_path.exists():
            continue
        short = data["name"].split(",", 1)[0]
        rows.append((short, human(bin_path.stat().st_size)))
    rows.sort(key=lambda r: r[0].lower())
    lines = ["| City | Compressed |", "|---|---|"]
    lines += [f"| {n} | {s} |" for n, s in rows]
    return "\n".join(lines)


def update_readme(table: str) -> None:
    readme = README.read_text()
    pattern = re.compile(re.escape(BEGIN) + r".*?" + re.escape(END), re.S)
    if not pattern.search(readme):
        sys.stderr.write(
            f"warning: markers {BEGIN} / {END} not found in README.md; not updating\n"
        )
        return
    replacement = f"{BEGIN}\n{table}\n{END}"
    new_readme = pattern.sub(replacement, readme)
    if new_readme != readme:
        README.write_text(new_readme)
        sys.stderr.write("README.md updated\n")
    else:
        sys.stderr.write("README.md already up to date\n")


def main() -> None:
    table = build_table()
    print(table)
    update_readme(table)


if __name__ == "__main__":
    main()
