#!/usr/bin/env python3
import argparse
import json
import re
import shutil
from pathlib import Path


def read_sha256(path: Path) -> str:
    text = path.read_text().strip()
    if not text:
        return ""
    return text.split()[0]


def main() -> None:
    parser = argparse.ArgumentParser(description="Prepare installer cache from release assets.")
    parser.add_argument("--assets-dir", required=True)
    parser.add_argument("--cache-dir", required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()

    assets_dir = Path(args.assets_dir)
    cache_dir = Path(args.cache_dir)
    version = args.version

    pattern = re.compile(r"^acm-installer-(.+)\.(tar\.xz|zip)$")
    platforms = {}

    for file in assets_dir.iterdir():
        match = pattern.match(file.name)
        if not match:
            continue
        platform = match.group(1)
        sha_path = assets_dir / f"{file.name}.sha256"
        if not sha_path.exists():
            raise SystemExit(f"Missing sha256 for {file.name}")

        sha256 = read_sha256(sha_path)
        size = file.stat().st_size

        platform_dir = cache_dir / "installer" / "versions" / version / platform
        platform_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy2(file, platform_dir / file.name)

        entry = platforms.setdefault(platform, {"files": {}})
        entry["files"][file.name] = {"sha256": sha256, "size": size}

    if not platforms:
        raise SystemExit("No installer assets found")

    checksums = {"version": version, "platforms": platforms}
    checksums_path = cache_dir / "installer" / "versions" / version / "checksums.json"
    checksums_path.write_text(json.dumps(checksums, indent=2, sort_keys=True))

    tags_dir = cache_dir / "installer" / "tags"
    tags_dir.mkdir(parents=True, exist_ok=True)
    (tags_dir / "latest").write_text(version)


if __name__ == "__main__":
    main()
