#!/usr/bin/env python3
"""Write KernelSU update metadata as UTF-8 bytes on every host platform."""

import argparse
import json
from pathlib import Path
import re
import stat
from urllib.parse import quote
import zipfile


R1_REPO = "qianxiao88/OhMyKeymint-R1"
R1_RAW = f"https://raw.githubusercontent.com/{R1_REPO}/master"
R1_NAME = re.compile(
    r"OhMyKeymint-R1-(?P<version>[0-9][A-Za-z0-9.+-]*)-"
    r"(?P<variant>lite|no-soter|full)-[0-9a-f]{7}\.zip"
)


def r1_updates(release: dict, asset_dir: Path) -> dict:
    updates = {}
    for asset in release["assets"]:
        name = asset["name"]
        if not name.startswith("OhMyKeymint-R1-") or not name.endswith(".zip"):
            continue
        match = R1_NAME.fullmatch(name)
        if match is None or asset.get("state") != "uploaded":
            raise ValueError(f"Invalid or incomplete R1 asset: {name}")
        variant = match["variant"]
        filename = f"update-{variant}.json"
        if filename in updates:
            raise ValueError(f"Duplicate R1 variant: {variant}")
        expected_url = (f"https://github.com/{R1_REPO}/releases/download/"
                        f"{quote(release['tag_name'], safe='')}/{quote(name, safe='')}")
        if asset["browser_download_url"] != expected_url:
            raise ValueError(f"Unexpected R1 download URL: {name}")
        path = asset_dir / name
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 512 * 1024**2:
            raise ValueError(f"Expected bounded regular R1 ZIP: {name}")
        with zipfile.ZipFile(path) as archive:
            infos = archive.infolist()
            props = [info for info in infos if info.filename == "module.prop"]
            if len(infos) > 10000 or len(props) != 1:
                raise ValueError(f"Expected exactly one root module.prop: {name}")
            info = props[0]
            if (info.orig_filename != info.filename or info.flag_bits & 1
                    or stat.S_IFMT(info.external_attr >> 16) not in (0, stat.S_IFREG)
                    or info.file_size > 65536):
                raise ValueError(f"Unsafe module.prop: {name}")
            # Read only the bounded property file; never extract or execute ZIP contents.
            with archive.open(info) as stream:
                data = stream.read(65537)
            if len(data) > 65536:
                raise ValueError(f"Oversized module.prop: {name}")
        properties = {}
        for line in data.decode("utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            key, separator, value = line.partition("=")
            key, value = key.strip(), value.strip()
            if not separator or not key or key in properties:
                raise ValueError(f"Invalid or duplicate module property: {name}")
            properties[key] = value
        expected = {"id": "oh_my_keymint", "name": name[:-4],
                    "version": f"{match['version']}-{variant}",
                    "updateJson": f"{R1_RAW}/{filename}"}
        if any(properties.get(key) != value for key, value in expected.items()):
            raise ValueError(f"R1 asset/module.prop mismatch: {name}")
        code = properties.get("versionCode", "")
        if not re.fullmatch(r"[0-9]{1,10}", code) or not 0 < int(code) <= 2147483647:
            raise ValueError(f"Invalid module versionCode: {name}")
        updates[filename] = {
            "version": properties["version"], "versionCode": int(code),
            "zipUrl": expected_url, "changelog": f"{R1_RAW}/changelog.md",
        }
    if set(updates) != {f"update-{variant}.json" for variant in ("lite", "no-soter", "full")}:
        raise ValueError("Expected all three uploaded R1 variants")
    return updates


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-json", required=True, type=Path)
    parser.add_argument("--version-code", type=int)
    parser.add_argument("--r1", action="store_true")
    parser.add_argument("--asset-dir", type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    release = json.loads(args.release_json.read_text(encoding="utf-8"))
    if release.get("draft") or release.get("prerelease"):
        return
    version = release["tag_name"]
    notes = release.get("body") or ""
    if not isinstance(version, str) or not version or not isinstance(notes, str):
        raise ValueError("Release tag and notes must be strings")
    if args.r1:
        if args.asset_dir is None:
            parser.error("--r1 requires --asset-dir")
        updates = r1_updates(release, args.asset_dir)
        for filename, update in updates.items():
            current = args.output_dir / filename
            if current.is_file():
                existing = json.loads(current.read_text(encoding="utf-8"))
                if existing["versionCode"] > update["versionCode"]:
                    return
        args.output_dir.mkdir(parents=True, exist_ok=True)
        for filename, update in updates.items():
            (args.output_dir / filename).write_bytes(
                (json.dumps(update, ensure_ascii=False, indent=2) + "\n").encode("utf-8"))
        notes = notes.replace("\r\n", "\n").replace("\r", "\n").rstrip("\n") + "\n"
        (args.output_dir / "changelog.md").write_bytes(notes.encode("utf-8"))
        return
    if args.version_code is None or args.version_code <= 0:
        raise ValueError("Version code must be positive")
    assets = [
        asset for asset in release["assets"]
        if "release" in asset["name"] and asset["name"].endswith(".zip")
        and asset.get("state") == "uploaded"
    ]
    if len(assets) != 1:
        raise ValueError("Expected exactly one uploaded release ZIP")
    update = {
        "version": version,
        "versionCode": args.version_code,
        "zipUrl": assets[0]["browser_download_url"],
        "changelog": "https://raw.githubusercontent.com/ITxiao6666/OhMyKeymint/changelog/changelog.md",
    }
    # Explicit bytes avoid Windows locale encoding and newline conversion. Never
    # pipe non-ASCII metadata through subprocess text mode without UTF-8 encoding.
    normalized_notes = notes.replace("\r\n", "\n").replace("\r", "\n").rstrip("\n") + "\n"
    encoded_notes = normalized_notes.encode("utf-8")
    encoded_update = (json.dumps(update, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "changelog.md").write_bytes(encoded_notes)
    (args.output_dir / "update.json").write_bytes(encoded_update)


if __name__ == "__main__":
    main()
