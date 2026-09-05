#!/usr/bin/env python3
"""Refresh staged file hashes after native code signing changes Mach-O bytes."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

from prepare_tool_bundle import BundleError, sha256_file


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bundle", required=True, type=Path)
    args = parser.parse_args()
    bundle = args.bundle.resolve()
    manifest_path = bundle / "manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    entries = manifest.get("files")
    if not isinstance(entries, list) or not entries:
        raise BundleError("Bundle manifest contains no files")
    recorded = {entry.get("path") for entry in entries if isinstance(entry, dict)}
    actual = {
        file_path.relative_to(bundle).as_posix()
        for file_path in bundle.rglob("*")
        if file_path.is_file() and file_path.name != "manifest.json"
    }
    if None in recorded or recorded != actual:
        raise BundleError("Refusing to refresh a manifest whose file set changed")
    for entry in entries:
        file_path = bundle / entry["path"]
        if file_path.is_symlink() or not file_path.is_file():
            raise BundleError(f"Manifest input is not a regular file: {entry['path']}")
        entry["size"] = file_path.stat().st_size
        entry["sha256"] = sha256_file(file_path)
        entry["executable"] = bool(file_path.stat().st_mode & 0o111)
    temporary = manifest_path.with_name(f".manifest.json.tmp-{os.getpid()}")
    temporary.write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    os.replace(temporary, manifest_path)
    print(f"Refreshed signed bundle manifest: {manifest_path}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (BundleError, OSError, KeyError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1)
