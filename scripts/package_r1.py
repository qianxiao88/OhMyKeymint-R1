#!/usr/bin/env python3
"""Repackage the pinned upstream Release; never compile or replace its native code."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import struct
import sys
import zipfile


ROOT = Path(__file__).resolve().parents[1]
UPSTREAM_COMMIT = "e0c4a096e5551816dacd60221e08b40eb2a4903b"
BASE_SHA256 = "b58618e8a6566f9dc0542132365607fc20ff49ee7169c7a2466cfea6576f7e92"
UPSTREAM_VERSION = "1.4.2"
BRAND = "1.3.8-301"
AUTHOR = "\u6d45\u7b11\u5450"
VARIANTS = ("lite", "no-soter", "full")
GIB = 1024**3
MAX_FILE = 128 * 1024**2
MAX_TOTAL = 512 * 1024**2
MAX_ENTRIES = 10000
HELPER_PATH = "arm64-v8a/r1-helper"
# Bytes and UNIX permission bits; directory records are deliberately regenerated.
EntryMap = dict[str, tuple[bytes, int]]


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def check_disk(path: Path, *, entry: bool = False) -> None:
    threshold = (6 if entry else 2) * GIB
    free = shutil.disk_usage(path).free
    if free < threshold:
        raise RuntimeError(f"Paused: {path} has {free} free bytes; requires {threshold}. "
                           "Keep existing releases; select storage with sufficient space.")


def safe_name(name: str, *, directory: bool = False) -> str:
    if directory and name.endswith("/"):
        name = name[:-1]
    if (not name or len(name.encode("utf-8")) > 240
            or not re.fullmatch(r"[A-Za-z0-9_./+\-]+", name)
            or any(part in ("", ".", "..") for part in name.split("/"))):
        raise ValueError(f"Unsafe archive path: {name!r}")
    return name


def check_tree(entries: EntryMap) -> None:
    if len(entries) > MAX_ENTRIES or sum(len(data) for data, _ in entries.values()) > MAX_TOTAL:
        raise ValueError("Payload exceeds entry count or total size limit")
    for name, (data, _) in entries.items():
        safe_name(name)
        if len(data) > MAX_FILE:
            raise ValueError(f"Entry exceeds size limit: {name}")
        parts = name.split("/")
        if any("/".join(parts[:i]) in entries for i in range(1, len(parts))):
            raise ValueError(f"File/directory collision: {name}")


def read_archive(stream: zipfile.ZipFile) -> EntryMap:
    infos = stream.infolist()
    if len(infos) > MAX_ENTRIES:
        raise ValueError("Archive exceeds entry count limit")
    seen: set[str] = set()
    total = 0
    # Validate all declared sizes and paths before allocating decompressed data.
    for info in infos:
        name = safe_name(info.filename, directory=info.is_dir())
        kind = stat.S_IFMT(info.external_attr >> 16)
        if (name in seen or info.orig_filename != info.filename
                or kind not in (0, stat.S_IFREG, stat.S_IFDIR)
                or (kind == stat.S_IFDIR and not info.is_dir())
                or (kind == stat.S_IFREG and info.is_dir()) or info.flag_bits & 1):
            raise ValueError(f"Duplicate, encrypted or non-regular archive entry: {name}")
        seen.add(name)
        total += info.file_size
        if info.file_size > MAX_FILE or total > MAX_TOTAL:
            raise ValueError("Archive exceeds expanded size limit")
    entries = {}
    for info in infos:
        if info.is_dir():
            continue
        # read() also verifies the ZIP CRC. The pinned digest authenticates the base.
        data = stream.read(info)
        mode = 0o755 if info.external_attr >> 16 & 0o111 else 0o644
        entries[info.filename] = (data, mode)
    check_tree(entries)
    for name in seen:
        parts = name.split("/")
        if any("/".join(parts[:i]) in entries for i in range(1, len(parts))):
            raise ValueError(f"File/directory collision: {name}")
    return entries


def load_base(path: Path) -> EntryMap:
    if path.stat().st_size > MAX_TOTAL:
        raise ValueError("Base ZIP exceeds compressed size limit")
    # Use the same open file for digest and ZIP reads.
    with path.open("rb") as stream:
        if hashlib.file_digest(stream, "sha256").hexdigest() != BASE_SHA256:
            raise ValueError("Unknown base ZIP: pinned upstream SHA256 mismatch")
        stream.seek(0)
        with zipfile.ZipFile(stream) as archive:
            entries = read_archive(archive)
    required = ("libs/arm64-v8a/keymint", "libs/arm64-v8a/inject", "service.sh",
                "customize.sh", "verify.sh", "module.prop", "webroot.manifest")
    if any(name not in entries for name in required):
        raise ValueError("Pinned base has an unsupported module layout")
    for name in required[:2]:
        validate_helper(entries[name][0])
    if not any(is_license(name) for name in entries):
        raise ValueError("Upstream license files are required")
    return entries


def validate_helper(data: bytes) -> None:
    if (len(data) < 64 or data[:7] != b"\x7fELF\x02\x01\x01"
            or struct.unpack_from("<H", data, 18)[0] != 183
            or struct.unpack_from("<H", data, 16)[0] not in (2, 3)
            or struct.unpack_from("<I", data, 20)[0] != 1
            or struct.unpack_from("<H", data, 52)[0] != 64):
        raise ValueError("Expected a little-endian ELF64 AArch64 executable/PIE")
    # e_machine/class cannot prove Android linkage, API level or device readiness.


def read_input(path: Path) -> bytes:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_FILE:
        raise ValueError(f"Expected a bounded regular input file: {path}")
    with path.open("rb") as stream:
        data = stream.read(MAX_FILE + 1)
    if len(data) > MAX_FILE:
        raise ValueError(f"Input grew beyond size limit: {path}")
    return data


def load_webroot(path: Path) -> EntryMap:
    if path.is_symlink() or not path.is_dir():
        raise ValueError(f"Expected WebUI build directory: {path}")
    entries: EntryMap = {}
    total = 0
    pending = [path]
    count = 0
    while pending:
        with os.scandir(pending.pop()) as children:
            for child in children:
                count += 1
                if count > MAX_ENTRIES:
                    raise ValueError("WebUI exceeds directory/file count limit")
                item = Path(child.path)
                safe_name("webroot/" + item.relative_to(path).as_posix())
                if child.is_symlink():
                    raise ValueError(f"WebUI symlink is unsupported: {item}")
                if child.is_dir(follow_symlinks=False):
                    pending.append(item)
                    continue
                name = "webroot/" + item.relative_to(path).as_posix()
                if name.endswith(".sha256"):
                    continue
                data = read_input(item)
                total += len(data)
                if total > MAX_TOTAL:
                    raise ValueError("WebUI exceeds total size limit")
                entries[name] = (data, 0o644)
    if any(name not in entries for name in ("webroot/index.html", "webroot/config.json")):
        raise ValueError(f"Incomplete WebUI build: {path}")
    check_tree(entries)
    return entries


def is_license(name: str) -> bool:
    upper = name.upper()
    return any(token in upper for token in ("LICENSE", "COPYING", "NOTICE", "THIRD_PARTY"))


def is_soter_asset(name: str, data: bytes) -> bool:
    return (not is_license(name) and not data.startswith(b"\x7fELF")
            and any(part.lower().startswith(("soter", "soterta")) for part in name.split("/")))


def boot_hook(template: bytes) -> bytes:
    begin = b"# BEGIN R1 BOOT HOOK\n"
    end = b"# END R1 BOOT HOOK\n"
    if template.count(begin) != 1 or template.count(end) != 1:
        raise ValueError("Missing or ambiguous R1 service hook markers")
    return begin + template.split(begin, 1)[1].split(end, 1)[0] + end


def replace_once(data: bytes, anchor: bytes, replacement: bytes) -> bytes:
    if data.count(anchor) != 1:
        raise ValueError(f"Unsupported upstream script anchor: {anchor!r}")
    return data.replace(anchor, replacement, 1)


def properties(base: bytes, stem: str, variant: str) -> bytes:
    values = {
        "name": stem, "version": f"{BRAND}-{variant}", "versionCode": "301",
        "author": AUTHOR,
        "description": "\u5f85\u542f\u52a8\u68c0\u67e5 | R1 " + variant + "; upstream 1.4.2 (precompiled)",
    }
    result = []
    found = set()
    for line in base.decode("utf-8").splitlines():
        key = line.split("=", 1)[0].strip()
        if key == "updateJson":
            continue
        if key in values:
            if key not in found:
                result.append(f"{key}={values[key]}")
                found.add(key)
        else:
            result.append(line)
    result.extend(f"{key}={value}" for key, value in values.items() if key not in found)
    return ("\n".join(result) + "\n").encode("utf-8")


def content_digest(entries: EntryMap, context: dict) -> str:
    digest = hashlib.sha256()
    digest.update(json.dumps(context, sort_keys=True, separators=(",", ":")).encode())
    # Length framing makes the sorted name/mode/content stream unambiguous.
    for name, (data, mode) in sorted(entries.items()):
        for part in (name.encode(), str(mode).encode(), data):
            digest.update(len(part).to_bytes(8, "big"))
            digest.update(part)
    return digest.hexdigest()


def add_checksums(entries: EntryMap) -> EntryMap:
    result = {name: entry for name, entry in entries.items() if not name.endswith(".sha256")}
    for name, (data, _) in list(result.items()):
        result[name + ".sha256"] = (sha256(data).encode("ascii"), 0o644)
    check_tree(result)
    return result


def make_variant(base: EntryMap, helper: bytes, webroot: EntryMap, variant: str,
                 service_template: bytes, packager_digest: str) -> tuple[str, EntryMap, dict]:
    if variant not in VARIANTS:
        raise ValueError(f"Unsupported variant: {variant}")
    validate_helper(helper)
    check_tree(base)
    check_tree(webroot)
    if any(not name.startswith("webroot/") for name in webroot):
        raise ValueError("WebUI entries must be under webroot/")
    removed = []
    entries: EntryMap = {}
    native = {}
    for name, (data, mode) in base.items():
        if name.endswith(".sha256") or name.startswith("webroot/") or name == "webroot.manifest":
            continue
        if data.startswith(b"\x7fELF"):
            native[name] = sha256(data)
        if variant != "full" and is_soter_asset(name, data):
            removed.append(name)
            continue
        entries[name] = (data, mode)
    reserved = {HELPER_PATH, "r1-build.json", "r1-extra.manifest", "r1-no-soter"}
    if reserved.intersection(entries):
        raise ValueError("Base ZIP contains reserved R1 paths")
    entries.update(webroot)
    entries[HELPER_PATH] = (helper, 0o755)
    entries["webroot.manifest"] = (("\n".join(sorted(webroot)) + "\n").encode(), 0o644)
    anchor = b'start_daemon "$MODDIR/daemon" "$STATE_DIR/keymint-daemon.pid"'
    service = replace_once(entries["service.sh"][0], anchor, boot_hook(service_template) + b"\n" + anchor)
    entries["service.sh"] = (service, 0o755)
    extras = sorted({HELPER_PATH, "r1-build.json"} | {n for n in entries if is_license(n)})
    if variant != "full":
        entries["r1-no-soter"] = (b"Disable via upstream native API before daemon startup.\n", 0o644)
        extras.append("r1-no-soter")
    entries["r1-extra.manifest"] = (("\n".join(sorted(extras)) + "\n").encode(), 0o644)
    # Preserve upstream extraction/verification logic; extend only the ZIP copy.
    anchor = b'while IFS= read -r asset || [ -n "$asset" ]; do'
    extraction = b'''extract "$ZIPFILE" 'r1-extra.manifest' "$MODPATH"
while IFS= read -r asset || [ -n "$asset" ]; do
  extract "$ZIPFILE" "$asset" "$MODPATH"
done < "$MODPATH/r1-extra.manifest"
chmod 0755 "$MODPATH/arm64-v8a" "$MODPATH/arm64-v8a/r1-helper"
chmod 0644 "$MODPATH/r1-build.json" "$MODPATH/r1-extra.manifest"

'''
    entries["customize.sh"] = (replace_once(entries["customize.sh"][0], anchor, extraction + anchor), 0o755)
    stem_prefix = f"OhMyKeymint-R1-{BRAND}-{variant}-"
    entries["module.prop"] = (properties(base["module.prop"][0], stem_prefix + "{content_hash}", variant), 0o644)
    context = {"base_zip_sha256": BASE_SHA256, "upstream_commit": UPSTREAM_COMMIT,
               "upstream_version": UPSTREAM_VERSION, "variant": variant,
               "packager_sha256": packager_digest, "service_template_sha256": sha256(service_template)}
    digest = content_digest(entries, context)
    short_hash = digest[-7:]
    stem = stem_prefix + short_hash
    manifest = dict(context, version=f"{BRAND}-{variant}", hash=short_hash,
                    content_sha256=digest, hash_kind="sha256-content-last7",
                    hash_scope="sorted pre-metadata payload; module name uses {content_hash}; context included",
                    native_rebuilt=False, upstream_native_sha256=native,
                    upstream_author=next((line[7:] for line in base["module.prop"][0].decode().splitlines()
                                          if line.startswith("author=")), ""),
                    removed_soter_assets=sorted(removed),
                    soter_policy="upstream" if variant == "full" else "disable-native-config-before-daemons",
                    soter_code_in_upstream_binaries=True,
                    validation={"on_device": "pending", "android_linkage": "pending",
                                "boot_status": "pending"},
                    helper_sha256=sha256(helper), helper_interface="monitor")
    entries["module.prop"] = (properties(base["module.prop"][0], stem, variant), 0o644)
    entries["r1-build.json"] = ((json.dumps(manifest, sort_keys=True, indent=2) + "\n").encode(), 0o644)
    for name, expected in native.items():
        if sha256(entries[name][0]) != expected:
            raise ValueError(f"Upstream native binary changed: {name}")
    return stem + ".zip", add_checksums(entries), manifest


def write_zip(path: Path, entries: EntryMap) -> None:
    check_tree(entries)
    # Exclusive creation preserves all earlier output, including partial runs.
    with path.open("xb") as stream:
        with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
            for name, (data, mode) in sorted(entries.items()):
                info = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = (stat.S_IFREG | mode) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, data)


def package(base_zip: Path, helper_path: Path, webroot_root: Path, output: Path) -> None:
    if output.exists() or output.is_symlink():
        raise FileExistsError("Output already exists; select a new output directory")
    if not output.parent.is_dir():
        raise ValueError("Output parent must be an existing directory")
    check_disk(output.parent, entry=True)
    base = load_base(base_zip)
    check_disk(output.parent)
    helper = read_input(helper_path)
    validate_helper(helper)
    service = read_input(ROOT / "template/service.sh")
    packager_digest = sha256(read_input(Path(__file__)))
    # Sequential variants bound peak memory; no extracted staging tree is created.
    output.mkdir()
    sums = []
    manifests = {}
    for variant in VARIANTS:
        check_disk(output)
        webroot = load_webroot(webroot_root / variant)
        name, entries, manifest = make_variant(base, helper, webroot, variant, service, packager_digest)
        check_disk(output)
        write_zip(output / name, entries)
        check_disk(output)
        sums.append(f"{file_digest(output / name)}  {name}\n")
        manifests[name] = manifest
        del entries, webroot
    with (output / "manifest.json").open("x", encoding="utf-8") as stream:
        json.dump(manifests, stream, sort_keys=True, indent=2)
        stream.write("\n")
    with (output / "SHA256SUMS").open("x", encoding="ascii") as stream:
        stream.write("".join(sums))
    check_disk(output)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-zip", required=True, type=Path)
    parser.add_argument("--helper", required=True, type=Path)
    parser.add_argument("--webroot-root", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        package(args.base_zip, args.helper, args.webroot_root, args.output)
    except (OSError, ValueError, RuntimeError, zipfile.BadZipFile) as error:
        print(f"R1 packaging stopped: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
