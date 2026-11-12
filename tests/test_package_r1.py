"""Synthetic source-build fixtures; no Cargo, install, or device execution."""

import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("package_r1", ROOT / "scripts/package_r1.py")
r1 = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(r1)


def elf(suffix=b""):
    data = bytearray(64)
    data[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HHI", data, 16, 3, 183, 1)
    struct.pack_into("<H", data, 52, 64)
    return bytes(data) + suffix


def source_entries(variant):
    entries = {
        "libs/arm64-v8a/keymint": (elf(variant.encode()), 0o755),
        "libs/arm64-v8a/inject": (elf(b"inject-" + variant.encode()), 0o755),
        r1.HELPER_PATH: (elf(b"helper"), 0o755),
        "service.sh": (b'"$R1_HELPER" monitor &\nstart_daemon\n', 0o755),
        "customize.sh": (b'extract "$ZIPFILE" payload.manifest "$MODPATH"\n', 0o755),
        "verify.sh": (b"original verification retained\n", 0o755),
        "META-INF/com/google/android/update-binary": (b"#!/sbin/sh\n", 0o755),
        "module.prop": (b"id=oh_my_keymint\nname=template\nversion=${versionName}\n"
                        b"versionCode=${versionCode}\nauthor=James Clef,ITxiao6666\n", 0o644),
        "LICENSE.md": (b"Original license and copyright", 0o644),
        "THIRD_PARTY_LICENSES/SOTER-local.txt": (b"Original Soter attribution", 0o644),
        "webroot/index.html": (b"<main>" + r1.module_name(variant).encode() + b"</main>", 0o644),
        "webroot/config.json": (b"{}", 0o644),
        "webroot/assets/app.js": (b'const name="' + r1.module_name(variant).encode() + b'";', 0o644),
        "webroot/old.js.sha256": (b"STALE", 0o644),
    }
    if variant != "lite":
        entries.update({"zygisk/arm64-v8a.so": (elf(b"pif"), 0o755),
                        "libs/arm64-v8a/libomk_engineer.so": (elf(b"engineer"), 0o755),
                        "libs/arm64-v8a/omk_engineer.dex": (b"dex\n035\x00fixture", 0o644)})
    return entries


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.stage = Path(self.directory.name) / "stage"
        self.stage.mkdir()

    def variant(self, variant="full", *, overrides=None, source=None):
        entries = source_entries(variant)
        entries.update(overrides or {})
        for name, (data, _) in entries.items():
            path = self.stage / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        return r1.finalize_variant(self.stage, variant,
                                  source=source or {"head": "a" * 40, "dirty": True},
                                  build_type="release", upstream_version="1.4.2",
                                  builder_sha256={"build.py": "fixture"})

    def test_archive_paths(self):
        for name in ("../x", "/abs", "a/../x", "a//b", "a/./b", "C:/x", "a\\b",
                     "a\nx", "a\x00x", "a*", "a?", "a[0]", "a b", "a/", "x" * 241):
            with self.subTest(name=name), self.assertRaises(ValueError):
                r1.safe_name(name)
        self.assertEqual(r1.safe_name("webroot/assets/a-b.1.js"), "webroot/assets/a-b.1.js")

    def read_zip(self, records):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, "w") as archive:
            for info, data in records:
                archive.writestr(info, data)
        stream.seek(0)
        with zipfile.ZipFile(stream) as archive:
            return r1.read_archive(archive)

    def test_archive_rejects_links_specials_and_traversal(self):
        for mode in (stat.S_IFLNK | 0o777, stat.S_IFIFO | 0o600):
            info = zipfile.ZipInfo("unsafe")
            info.create_system = 3
            info.external_attr = mode << 16
            with self.assertRaises(ValueError):
                self.read_zip([(info, b"target")])
        with self.assertRaises(ValueError):
            self.read_zip([("../bad", b"x")])

    def test_archive_collision_duplicates_and_limits(self):
        for records in ([("a", b"x"), ("a/b", b"x")], [("a", b"x"), ("a/b/", b"")]):
            with self.assertRaises(ValueError):
                self.read_zip(records)
        with self.assertWarns(UserWarning), self.assertRaises(ValueError):
            self.read_zip([("same", b"a"), ("same", b"b")])
        for option, limit, records in (("MAX_FILE", 3, [("large", b"1234")]),
                                       ("MAX_TOTAL", 3, [("a", b"12"), ("b", b"34")]),
                                       ("MAX_ENTRIES", 1, [("a", b"1"), ("b", b"2")])):
            with patch.object(r1, option, limit), self.assertRaises(ValueError):
                self.read_zip(records)

    def test_elf_header_checks(self):
        r1.validate_helper(elf())
        for at, value in ((4, 1), (5, 2), (18, 62), (16, 1), (52, 0)):
            data = bytearray(elf())
            data[at] = value
            with self.subTest(at=at), self.assertRaises(ValueError):
                r1.validate_helper(bytes(data))
        with self.assertRaises(ValueError):
            r1.validate_helper(b"short")

    def test_three_variants_metadata_and_payload_differences(self):
        results = {}
        for variant in r1.VARIANTS:
            with self.subTest(variant=variant), tempfile.TemporaryDirectory() as directory:
                stage = self.stage
                self.stage = Path(directory)
                try:
                    name, entries, manifest = self.variant(variant)
                finally:
                    self.stage = stage
                results[variant] = manifest["hash"]
                self.assertRegex(name, rf"^OhMyKeymint-R1-1\.3\.8-302-{variant}-[0-9a-f]{{7}}\.zip$")
                props = dict(line.split("=", 1) for line in entries["module.prop"][0].decode().splitlines())
                self.assertEqual(props["name"], name[:-4])
                self.assertEqual(props["author"], "James Clef,ITxiao6666," + r1.AUTHOR)
                self.assertEqual(props["versionCode"], "302")
                self.assertEqual(props["version"], f"1.3.8-302-{variant}")
                self.assertTrue(manifest["native_rebuilt"])
                self.assertEqual(manifest["features"], list(r1.FEATURES[variant]))
                self.assertEqual(manifest["injector_features"], list(r1.INJECTOR_FEATURES[variant]))
                self.assertEqual(manifest["source"], {"head": "a" * 40, "dirty": True})
                self.assertTrue(all(value == "pending" for value in manifest["validation"].values()))
                self.assertEqual(json.loads(entries["r1-build.json"][0]), manifest)
                self.assertEqual("zygisk/arm64-v8a.so" in entries, variant != "lite")
                self.assertEqual("libs/arm64-v8a/omk_engineer.dex" in entries, variant != "lite")
                self.assertNotIn("r1-no-soter", entries)
                self.assertIn(name[:-4].encode(), entries["webroot/assets/app.js"][0])
                self.assertNotIn(r1.HASH_PLACEHOLDER.encode(), entries["webroot/assets/app.js"][0])
                self.assertIn("THIRD_PARTY_LICENSES/SOTER-local.txt", entries)
        self.assertEqual(len(set(results.values())), 3)

    def test_author_append_is_idempotent(self):
        for original in ("Original Author,Contributor", "", r1.AUTHOR):
            result = r1.properties(f"author={original}\n".encode(), "R1", "full")
            result = r1.properties(result, "R1", "full")
            authors = next(line[7:] for line in result.decode().splitlines() if line.startswith("author="))
            self.assertEqual(authors.split(",").count(r1.AUTHOR), 1)
            if original:
                self.assertTrue(authors.startswith(original))

    def test_hash_recomputes_with_placeholder_names_and_manifest_context(self):
        name, entries, manifest = self.variant()
        self.assertEqual(manifest["hash"], manifest["content_sha256"][-7:])
        self.assertTrue(name.endswith(manifest["hash"] + ".zip"))
        context = {key: value for key, value in manifest.items()
                   if key not in ("hash", "content_sha256", "hash_kind", "hash_scope")}
        snapshot = r1.normalized_payload(entries, "full", manifest["hash"])
        self.assertEqual(r1.content_digest(snapshot, context), manifest["content_sha256"])
        self.assertEqual((name, entries, manifest), self.variant())
        changed = self.variant(overrides={"webroot/assets/new.js": (b"new", 0o644)})
        self.assertNotEqual(manifest["hash"], changed[2]["hash"])
        changed = self.variant(source={"head": "a" * 40, "dirty": False})
        self.assertNotEqual(manifest["hash"], changed[2]["hash"])

    def test_checksums_and_manifests_cover_final_payload(self):
        _, entries, _ = self.variant()
        self.assertNotIn("webroot/old.js.sha256", entries)
        for name, (data, _) in entries.items():
            if not name.endswith(".sha256"):
                self.assertEqual(entries[name + ".sha256"][0], hashlib.sha256(data).hexdigest().encode())
        self.assertFalse(any(name.endswith(".sha256.sha256") for name in entries))
        web = entries["webroot.manifest"][0].decode().splitlines()
        self.assertEqual(web, sorted(name for name in entries if name.startswith("webroot/")
                                     and not name.endswith(".sha256")))
        payload = entries["payload.manifest"][0].decode().splitlines()
        expected = sorted(name for name in entries if name not in ("customize.sh", "verify.sh", "payload.manifest")
                          and not name.startswith("META-INF/") and not name.endswith(".sha256"))
        self.assertEqual(payload, expected)
        self.assertIn("r1-build.json", payload)
        self.assertIn(r1.HELPER_PATH, payload)

    def test_source_variants_reject_runtime_disable_or_optional_lite_native(self):
        with self.assertRaisesRegex(ValueError, "compile time"):
            self.variant(overrides={"r1-no-soter": (b"disable", 0o644)})
        (self.stage / "r1-no-soter").unlink()
        with self.assertRaisesRegex(ValueError, "compile time"):
            self.variant(overrides={"service.sh": (b"--webui-set-soter-local-base64", 0o755)})
        with tempfile.TemporaryDirectory() as directory:
            self.stage = Path(directory)
            with self.assertRaisesRegex(ValueError, "optional native"):
                self.variant("lite", overrides={"zygisk/arm64-v8a.so": (elf(), 0o755)})

    def test_incomplete_native_and_mismatched_ui_rejected(self):
        with self.assertRaisesRegex(ValueError, "placeholder VITE_R1_NAME"):
            self.variant(overrides={"webroot/index.html": (b"old", 0o644),
                                    "webroot/assets/app.js": (b"old", 0o644)})
        with self.assertRaisesRegex(ValueError, "ELF64 AArch64"):
            self.variant(overrides={r1.HELPER_PATH: (b"old native", 0o755)})

    def test_zip_reproducible_permissions_and_preserves_existing_outputs(self):
        directory = Path(self.directory.name)
        first, second = directory / "first.zip", directory / "second.zip"
        entries = {"script.sh": (b"echo test\n", 0o755), "data.json": (b"{}", 0o644)}
        r1.write_zip(first, entries)
        r1.write_zip(second, dict(reversed(list(entries.items()))))
        self.assertEqual(first.read_bytes(), second.read_bytes())
        previous = first.read_bytes()
        with self.assertRaises(FileExistsError):
            r1.write_zip(first, {})
        self.assertEqual(first.read_bytes(), previous)
        with zipfile.ZipFile(first) as archive:
            for name, (_, mode) in entries.items():
                info = archive.getinfo(name)
                self.assertEqual(info.date_time, r1.ZIP_TIMESTAMP)
                self.assertEqual(info.external_attr >> 16 & 0o777, mode)
                self.assertEqual(info.create_system, 3)

    def test_identical_zip_reuse_preserves_old_bytes_and_conflicts_are_kept(self):
        path = Path(self.directory.name) / "release.zip"
        entries = {"script.sh": (b"echo test\n", 0o755), "data.json": (b"{}", 0o644)}
        self.assertFalse(r1.write_zip_or_reuse(path, entries))
        previous = path.read_bytes()
        timestamp = path.stat().st_mtime_ns
        self.assertTrue(r1.write_zip_or_reuse(path, dict(reversed(list(entries.items())))))
        self.assertEqual(path.read_bytes(), previous)
        self.assertEqual(path.stat().st_mtime_ns, timestamp)
        with self.assertRaisesRegex(FileExistsError, "differs; kept unchanged"):
            r1.write_zip_or_reuse(path, {"script.sh": (b"changed", 0o755)})
        self.assertEqual(path.read_bytes(), previous)
        self.assertFalse(list(path.parent.glob(".r1-compare-*")))
        link = path.parent / "link.zip"
        link.symlink_to(path)
        with self.assertRaisesRegex(FileExistsError, "not a regular ZIP"):
            r1.write_zip_or_reuse(link, entries)
        self.assertEqual(path.read_bytes(), previous)

    def test_webroot_symlink_incomplete_and_bounds(self):
        with self.assertRaisesRegex(ValueError, "Incomplete"):
            r1.load_webroot(self.stage)
        (self.stage / "index.html").write_bytes(b"index")
        (self.stage / "config.json").write_bytes(b"{}")
        (self.stage / "stale.sha256").write_bytes(b"old")
        self.assertNotIn("webroot/stale.sha256", r1.load_webroot(self.stage))
        (self.stage / "link").symlink_to(self.stage / "index.html")
        with self.assertRaisesRegex(ValueError, "symlink"):
            r1.load_webroot(self.stage)
        (self.stage / "link").unlink()
        with patch.object(r1, "MAX_ENTRIES", 1), self.assertRaisesRegex(ValueError, "count limit"):
            r1.load_webroot(self.stage)

    def test_skroot_wraps_finalized_payload_with_strict_manifest_and_same_identity(self):
        name, payload, module = self.variant()
        sk_name, entries, info = r1.finalize_skroot_variant(
            payload, loader=elf(b"loader"), worker=elf(b"worker"), variant="full", module_manifest=module)
        self.assertEqual(sk_name, name)
        manifest = json.loads(entries["omk-payload.json"][0])
        self.assertEqual(set(manifest), {"schema", "id32", "version", "variant", "files"})
        self.assertEqual(manifest["id32"], r1.SKROOT_ID32)
        self.assertEqual(manifest["version"], "1.3.8-302-full")
        self.assertEqual(manifest["schema"], 1)
        self.assertEqual(manifest["files"], [{"path": path, "mode": mode, "sha256": r1.sha256(data)}
                                            for path, (data, mode) in sorted(payload.items())])
        for path, original in payload.items():
            self.assertEqual(entries["payload/" + path], original)
        self.assertEqual(info["hash"], module["hash"])
        self.assertEqual(info["native_rebuilt"], True)
        self.assertTrue(all(value == "pending" for value in info["validation"].values()))
        snapshot = {key: value for key, value in entries.items()
                    if key != "skroot-build.json" and (key.startswith("payload/") or not key.endswith(".sha256"))}
        self.assertEqual(r1.content_digest(snapshot, {}), info["transport_content_sha256"])
        self.assertNotIn("omk-skroot-worker", entries)
        self.assertEqual([path for path, (data, _) in entries.items()
                          if "/" not in path and data.startswith(b"\x7fELF")], ["libomk_skroot_loader.so"])
        for path in ("libomk_skroot_loader.so", "skroot/omk-skroot-worker"):
            self.assertEqual(entries[path][1], 0o755)
        for path in ("libomk_skroot_loader.so", "skroot/omk-skroot-worker", "omk-payload.json", "skroot-build.json"):
            self.assertEqual(entries[path + ".sha256"][0], r1.sha256(entries[path][0]).encode())

    def test_skroot_rejects_mismatched_or_stale_payload(self):
        _, payload, module = self.variant()
        with self.assertRaisesRegex(ValueError, "matching finalized"):
            r1.finalize_skroot_variant(payload, loader=elf(), worker=elf(),
                                      variant="lite", module_manifest=module)
        payload["module.prop.sha256"] = (b"stale", 0o644)
        with self.assertRaisesRegex(ValueError, "checksum is missing or stale"):
            r1.finalize_skroot_variant(payload, loader=elf(), worker=elf(),
                                      variant="full", module_manifest=module)

    def test_old_precompiled_cli_is_removed(self):
        self.assertFalse(hasattr(r1, "load_base"))
        self.assertFalse(hasattr(r1, "make_variant"))
        self.assertFalse(hasattr(r1, "BASE_SHA256"))
        with patch.object(r1.os, "execv") as execute, patch.object(r1.sys, "argv", ["package_r1.py", "--variant", "all"]):
            r1.main()
        self.assertEqual(execute.call_args.args[1][-2:], ["--variant", "all"])
        self.assertTrue(execute.call_args.args[1][1].endswith("build.py"))


if __name__ == "__main__":
    unittest.main()
