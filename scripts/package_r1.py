#!/usr/bin/env python3
"""Deterministic R1 source-build packaging helpers; CLI delegates to build.py."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import struct
import sys
import tempfile
import zipfile


ROOT = Path(__file__).resolve().parents[1]
VERSION = "1.3.8"
VERSION_CODE = 303
BRAND = f"{VERSION}-{VERSION_CODE}"
AUTHOR = "\u6d45\u7b11\u5450"
VARIANTS = ("full", "no-soter", "lite")
FEATURES = {"full": ("soter", "pif", "local-rkp"),
            "no-soter": ("pif", "local-rkp"), "lite": ()}
INJECTOR_FEATURES = {"full": ("soter",), "no-soter": (), "lite": ()}
HELPER_PATH = "arm64-v8a/r1-helper"
SKROOT_ID32 = "3e5017f4de9b4d86a25d7d320fbd29e2"
HASH_PLACEHOLDER = "{content_hash}"
GIB = 1024**3
MAX_FILE = 128 * 1024**2
MAX_TOTAL = 512 * 1024**2
MAX_ENTRIES = 10000
ZIP_TIMESTAMP = (2020, 1, 1, 0, 0, 0)
# Bytes and normalized UNIX permission bits; directory records are omitted.
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
    for name, (data, mode) in entries.items():
        safe_name(name)
        if mode not in (0o644, 0o755):
            raise ValueError(f"Non-normalized file mode: {name}: {mode:o}")
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
        if not info.is_dir():
            mode = 0o755 if info.external_attr >> 16 & 0o111 else 0o644
            entries[info.filename] = (stream.read(info), mode)
    check_tree(entries)
    for name in seen:
        parts = name.split("/")
        if any("/".join(parts[:i]) in entries for i in range(1, len(parts))):
            raise ValueError(f"File/directory collision: {name}")
    return entries


def validate_helper(data: bytes) -> None:
    if (len(data) < 64 or data[:7] != b"\x7fELF\x02\x01\x01"
            or struct.unpack_from("<H", data, 18)[0] != 183
            or struct.unpack_from("<H", data, 16)[0] not in (2, 3)
            or struct.unpack_from("<I", data, 20)[0] != 1
            or struct.unpack_from("<H", data, 52)[0] != 64):
        raise ValueError("Expected a little-endian ELF64 AArch64 executable/PIE")
    # The header does not establish Android linkage, API level or device readiness.


def read_input(path: Path) -> bytes:
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_FILE:
        raise ValueError(f"Expected a bounded regular input file: {path}")
    with path.open("rb") as stream:
        data = stream.read(MAX_FILE + 1)
    if len(data) > MAX_FILE:
        raise ValueError(f"Input grew beyond size limit: {path}")
    return data


def normalized_mode(name: str, data: bytes) -> int:
    if (data.startswith(b"\x7fELF") or name.endswith(".sh")
            or name in ("daemon", "daemon-injector", "META-INF/com/google/android/update-binary")):
        return 0o755
    return 0o644


def load_tree(path: Path, *, prefix: str = "") -> EntryMap:
    if path.is_symlink() or not path.is_dir():
        raise ValueError(f"Expected payload directory: {path}")
    entries: EntryMap = {}
    pending = [path]
    count = total = 0
    while pending:
        with os.scandir(pending.pop()) as children:
            for child in children:
                count += 1
                if count > MAX_ENTRIES:
                    raise ValueError("Payload exceeds directory/file count limit")
                item = Path(child.path)
                name = prefix + item.relative_to(path).as_posix()
                safe_name(name)
                if child.is_symlink():
                    raise ValueError(f"Payload symlink is unsupported: {item}")
                if child.is_dir(follow_symlinks=False):
                    pending.append(item)
                    continue
                if name.endswith(".sha256"):
                    continue
                data = read_input(item)
                total += len(data)
                if total > MAX_TOTAL:
                    raise ValueError("Payload exceeds total size limit")
                entries[name] = (data, normalized_mode(name, data))
    check_tree(entries)
    return entries


def load_webroot(path: Path) -> EntryMap:
    entries = load_tree(path, prefix="webroot/")
    if any(name not in entries for name in ("webroot/index.html", "webroot/config.json")):
        raise ValueError(f"Incomplete WebUI build: {path}")
    return entries


def is_license(name: str) -> bool:
    return any(token in name.upper() for token in ("LICENSE", "COPYING", "NOTICE", "THIRD_PARTY"))


def module_name(variant: str, digest: str = HASH_PLACEHOLDER) -> str:
    if variant not in VARIANTS:
        raise ValueError(f"Unsupported variant: {variant}")
    return f"OhMyKeymint-R1-{BRAND}-{variant}-{digest}"


def properties(base: bytes, stem: str, variant: str) -> bytes:
    lines = base.decode("utf-8").splitlines()
    original = next((line[7:].strip() for line in lines if line.startswith("author=")), "")
    authors = [author.strip() for author in original.split(",") if author.strip()]
    if AUTHOR not in authors:
        authors.append(AUTHOR)
    values = {
        "name": stem, "version": f"{BRAND}-{variant}", "versionCode": str(VERSION_CODE),
        "author": ",".join(authors),
        "description": "OMK-R1 \u5f85\u542f\u52a8\u68c0\u67e5\u3002\u8fd0\u884c\u72b6\u6001\u53ca\u4e91\u7aef\u8fde\u63a5\u7ed3\u679c\u5c06\u5728\u542f\u52a8\u540e\u66f4\u65b0\u3002",
        "updateJson": ("https://raw.githubusercontent.com/qianxiao88/OhMyKeymint-R1/"
                       f"master/update-{variant}.json"),
    }
    result, found = [], set()
    for line in lines:
        key = line.split("=", 1)[0].strip()
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


def normalized_payload(entries: EntryMap, variant: str, content_hash: str) -> EntryMap:
    result = {name: entry for name, entry in entries.items()
              if not name.endswith(".sha256") and name != "r1-build.json"}
    final_name = module_name(variant, content_hash).encode()
    placeholder_name = module_name(variant).encode()
    for name, (data, mode) in list(result.items()):
        if name == "module.prop" or name.startswith("webroot/"):
            result[name] = (data.replace(final_name, placeholder_name), mode)
    return result


def finalize_variant(stage_dir: Path, variant: str, *, source: dict, build_type: str,
                     upstream_version: str, builder_sha256: dict) -> tuple[str, EntryMap, dict]:
    """Seal freshly compiled staging files; source provenance is part of the hash."""
    if variant not in VARIANTS:
        raise ValueError(f"Unsupported variant: {variant}")
    entries = load_tree(stage_dir)
    required = ("libs/arm64-v8a/keymint", "libs/arm64-v8a/inject", HELPER_PATH,
                "service.sh", "customize.sh", "module.prop", "webroot/index.html",
                "webroot/config.json")
    if any(name not in entries for name in required):
        raise ValueError("Source build has an incomplete module layout")
    for name in required[:3]:
        validate_helper(entries[name][0])
    pif_paths = ("zygisk/arm64-v8a.so", "libs/arm64-v8a/libomk_engineer.so",
                 "libs/arm64-v8a/omk_engineer.dex")
    if variant == "lite":
        forbidden = [name for name in entries if name.startswith("zygisk/")
                     or (name.startswith("libs/") and "engineer" in name.lower())]
        if forbidden:
            raise ValueError(f"Lite contains optional native payloads: {', '.join(sorted(forbidden))}")
    elif any(name not in entries for name in pif_paths):
        raise ValueError("Non-lite source build is missing its PIF/engineer payload")
    else:
        for name in pif_paths[:2]:
            validate_helper(entries[name][0])
        if not entries[pif_paths[2]][0].startswith(b"dex\n"):
            raise ValueError("Engineer bridge requires a rebuilt DEX payload")
    if "r1-no-soter" in entries or b"--webui-set-soter-local-base64" in entries["service.sh"][0]:
        raise ValueError("Source variants must select Soter at compile time")
    if not any(is_license(name) for name in entries):
        raise ValueError("Project and third-party license files are required")
    original_prop = entries["module.prop"][0]
    entries["module.prop"] = (properties(original_prop, module_name(variant), variant), 0o644)
    web_names = sorted(name for name in entries if name.startswith("webroot/"))
    entries["webroot.manifest"] = (("\n".join(web_names) + "\n").encode(), 0o644)
    excluded = {"customize.sh", "verify.sh", "payload.manifest", "r1-extra.manifest"}
    payload = sorted({"r1-build.json"} | {name for name in entries
                     if name not in excluded and not name.startswith("META-INF/")})
    entries.pop("r1-extra.manifest", None)
    entries["payload.manifest"] = (("\n".join(payload) + "\n").encode(), 0o644)
    placeholder_name = module_name(variant).encode()
    if not any(placeholder_name in data for name, (data, _) in entries.items()
               if name.startswith("webroot/")):
        raise ValueError("WebUI must be built with the variant's placeholder VITE_R1_NAME")
    native = {name: sha256(data) for name, (data, _) in entries.items() if data.startswith(b"\x7fELF")}
    context = {
        "version": f"{BRAND}-{variant}", "version_code": VERSION_CODE, "variant": variant,
        "features": list(FEATURES[variant]), "injector_features": list(INJECTOR_FEATURES[variant]),
        "native_rebuilt": True, "source": source, "upstream_version": upstream_version,
        "build_type": build_type, "target": "aarch64-linux-android",
        "builder_sha256": builder_sha256, "native_sha256": native,
        "helper_sha256": sha256(entries[HELPER_PATH][0]), "helper_interface": "monitor",
        "validation": {"on_device": "pending", "android_linkage": "pending", "boot_status": "pending"},
    }
    digest = content_digest(normalized_payload(entries, variant, HASH_PLACEHOLDER), context)
    short_hash = digest[-7:]
    stem = module_name(variant, short_hash)
    for name, (data, mode) in list(entries.items()):
        if name == "module.prop" or name.startswith("webroot/"):
            entries[name] = (data.replace(placeholder_name, stem.encode()), mode)
    manifest = dict(context, hash=short_hash, content_sha256=digest,
                    hash_kind="sha256-content-last7",
                    hash_scope={"payload": "all sorted path/mode/bytes with length framing",
                                "exclude": ["r1-build.json", "*.sha256"],
                                "normalize": {"module.prop:name and webroot module name": placeholder_name.decode()},
                                "context": "manifest fields except hash/content_sha256/hash_kind/hash_scope",
                                "webui_asset_names": "retained from placeholder-name Vite build"})
    entries["r1-build.json"] = ((json.dumps(manifest, sort_keys=True, indent=2) + "\n").encode(), 0o644)
    return stem + ".zip", add_checksums(entries), manifest


def finalize_skroot_variant(payload: EntryMap, *, loader: bytes, worker: bytes,
                            variant: str, module_manifest: dict) -> tuple[str, EntryMap, dict]:
    """Wrap a finalized module without changing its version, name or hash."""
    validate_helper(loader)
    validate_helper(worker)
    check_tree(payload)
    if (variant not in VARIANTS or module_manifest.get("variant") != variant
            or module_manifest.get("version") != f"{BRAND}-{variant}"
            or not module_manifest.get("native_rebuilt")):
        raise ValueError("SKRootPro requires a matching finalized source-built payload")
    digest = module_manifest.get("hash", "")
    if not re.fullmatch(r"[0-9a-f]{7}", digest):
        raise ValueError("SKRootPro payload content hash must contain seven hexadecimal digits")
    for name, (data, _) in payload.items():
        if not name.endswith(".sha256"):
            if payload.get(name + ".sha256", (None,))[0] != sha256(data).encode():
                raise ValueError(f"SKRootPro payload checksum is missing or stale: {name}")
    manifest = {"schema": 1, "id32": SKROOT_ID32, "version": f"{BRAND}-{variant}",
                "variant": variant,
                "files": [{"path": name, "mode": mode, "sha256": sha256(data)}
                          for name, (data, mode) in sorted(payload.items())]}
    entries = {"payload/" + name: entry for name, entry in payload.items()}
    entries.update({"libomk_skroot_loader.so": (loader, 0o755),
                    "skroot/omk-skroot-worker": (worker, 0o755),
                    "omk-payload.json": ((json.dumps(manifest, sort_keys=True, indent=2) + "\n").encode(), 0o644)})
    transport_digest = content_digest(entries, {})
    build_info = {
        "schema": 1, "id32": SKROOT_ID32, "version": f"{BRAND}-{variant}",
        "variant": variant, "hash": digest, "module_content_sha256": module_manifest["content_sha256"],
        "native_rebuilt": True, "features": module_manifest["features"],
        "source": module_manifest["source"], "target": "aarch64-linux-android",
        "transport_content_sha256": transport_digest,
        "transport_hash_scope": "sorted path/mode/bytes including payload sidecars, before skroot-build.json and transport checksum sidecars; empty context",
        "filename_hash_scope": "standard module's normalized content hash; payload module/UI names unchanged",
        "loader_sha256": sha256(loader), "worker_sha256": sha256(worker),
        "validation": {"on_device": "pending", "android_linkage": "pending",
                       "skrootpro_loading": "pending", "policy_preflight": "pending"},
    }
    entries["skroot-build.json"] = ((json.dumps(build_info, sort_keys=True, indent=2) + "\n").encode(), 0o644)
    for name in ("libomk_skroot_loader.so", "skroot/omk-skroot-worker", "omk-payload.json", "skroot-build.json"):
        entries[name + ".sha256"] = (sha256(entries[name][0]).encode(), 0o644)
    check_tree(entries)
    return module_name(variant, digest) + ".zip", entries, build_info


def write_zip(path: Path, entries: EntryMap) -> None:
    check_tree(entries)
    # Exclusive creation preserves user archives and outputs of previous runs.
    with path.open("xb") as stream:
        with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for name, (data, mode) in sorted(entries.items()):
                info = zipfile.ZipInfo(name, date_time=ZIP_TIMESTAMP)
                info.create_system = 3
                info.external_attr = (stat.S_IFREG | mode) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, data, compresslevel=9)


def write_zip_or_reuse(path: Path, entries: EntryMap) -> bool:
    """Reuse only byte-identical deterministic output; never replace a prior ZIP."""
    check_tree(entries)
    try:
        write_zip(path, entries)
        return False
    except FileExistsError:
        if path.is_symlink() or not path.is_file():
            raise FileExistsError(f"Existing output is not a regular ZIP: {path}") from None
    # A temporary deterministic ZIP compares compressed bytes, timestamps and modes,
    # rather than accepting a merely equivalent archive with different metadata.
    with tempfile.TemporaryDirectory(prefix=".r1-compare-", dir=path.parent) as directory:
        candidate = Path(directory) / path.name
        write_zip(candidate, entries)
        with path.open("rb") as previous, candidate.open("rb") as proposed:
            while True:
                old_chunk, new_chunk = previous.read(1024 * 1024), proposed.read(1024 * 1024)
                if old_chunk != new_chunk:
                    raise FileExistsError(f"Existing output differs; kept unchanged: {path}")
                if not old_chunk:
                    return True


def main() -> int:
    # There is one source-build CLI: no --base-zip or precompiled native path.
    os.execv(sys.executable, [sys.executable, os.fspath(ROOT / "build.py"), *sys.argv[1:]])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
