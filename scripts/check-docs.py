#!/usr/bin/env python3
"""Check tracked UTF-8 text and local Markdown links from the repository root."""
from pathlib import Path
import re
import subprocess
from urllib.parse import unquote


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    paths = subprocess.check_output(
        ["git", "ls-files", "-z"], cwd=root
    ).decode("utf-8").split("\0")
    errors = []
    links = 0
    text_files = 0
    for name in filter(None, paths):
        path = root / name
        if not path.is_file():
            errors.append(f"Missing tracked file: {name}")
            continue
        data = path.read_bytes()
        if b"\0" in data:
            continue
        try:
            source = data.decode("utf-8")
        except UnicodeDecodeError:
            continue
        text_files += 1
        if data.startswith(b"\xef\xbb\xbf"):
            errors.append(f"UTF-8 BOM: {name}")
        if b"\r\n" in data and b"\n" in data.replace(b"\r\n", b""):
            errors.append(f"Mixed line endings: {name}")
        if path.suffix not in (".ps1", ".cmd") and b"\r" in data:
            errors.append(f"Text file must use LF: {name}")
        for number, line in enumerate(source.splitlines(), 1):
            if re.search("[\u3000-\u303f\u3400-\u9fff\uff00-\uffef]", line):
                errors.append(f"Prohibited character: {name}:{number}")
            if re.search(r"\b[A-Za-z]:[/\\]", line):
                errors.append(f"Absolute machine path: {name}:{number}")
        if path.suffix != ".md":
            continue
        for target in re.findall(r"!?\[[^\]]*\]\(([^\s)]+)\)", source):
            if re.match(r"[A-Za-z][A-Za-z0-9+.-]*:", target):
                continue
            local_path = unquote(target.split("#", 1)[0])
            if local_path and not (path.parent / local_path).exists():
                errors.append(f"Broken link: {name}: {target}")
            links += 1
    readme_lines = len((root / "README.md").read_text(encoding="utf-8").splitlines())
    if readme_lines >= 200:
        errors.append(f"README must stay below 200 lines: {readme_lines}")
    print(f"Tracked UTF-8 text files: {text_files}")
    print(f"Local Markdown links: {links}; README lines: {readme_lines}")
    print(f"Errors: {len(errors)}")
    for error in errors:
        print(error)
    return int(bool(errors))


if __name__ == "__main__":
    raise SystemExit(main())
