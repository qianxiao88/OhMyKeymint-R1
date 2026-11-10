"""Small synthetic fixtures only: no upstream build, install, or device execution."""

import base64
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
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


def elf():
    data = bytearray(64)
    data[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HHI", data, 16, 3, 183, 1)
    struct.pack_into("<H", data, 52, 64)
    return bytes(data)


def base_entries():
    return {
        "libs/arm64-v8a/keymint": (elf(), 0o755),
        "libs/arm64-v8a/inject": (elf() + b"inject", 0o755),
        "zygisk/arm64-v8a.so": (elf() + b"zygisk", 0o755),
        "libs/arm64-v8a/libsoter.so": (elf() + b"Soter core retained", 0o755),
        "soter/standalone.conf": (b"old Soter asset", 0o644),
        "LICENSE.md": (b"Original license and copyright", 0o644),
        "THIRD_PARTY_LICENSES/soter.txt": (b"Original third party author", 0o644),
        "service.sh": (b'BASE BOOT\nstart_daemon "$MODDIR/daemon" "$STATE_DIR/keymint-daemon.pid"\nTAIL\n', 0o755),
        "customize.sh": (b'BASE INSTALL\nwhile IFS= read -r asset || [ -n "$asset" ]; do\n  extract "$ZIPFILE" "$asset" "$MODPATH"\ndone < "$MODPATH/webroot.manifest"\n', 0o755),
        "verify.sh": (b"original verification retained", 0o755),
        "module.prop": (b"id=oh_my_keymint\nname=Original\nversion=1.4.2\nversionCode=800\nauthor=James Clef,ITxiao6666\ndescription=Original\nupdateJson=https://example.org/update\n", 0o644),
        "webroot/old.js": (b"STALE", 0o644),
        "webroot/old.js.sha256": (b"STALE HASH", 0o644),
        "webroot.manifest": (b"webroot/old.js\n", 0o644),
        "module.prop.sha256": (b"STALE HASH", 0o644),
    }


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.base = base_entries()
        self.web = {"webroot/index.html": (b"<main>new</main>", 0o644),
                    "webroot/config.json": (b"{}", 0o644)}
        self.service = (ROOT / "template/service.sh").read_bytes()

    def variant(self, variant="full", **overrides):
        args = dict(base=self.base, helper=elf(), webroot=self.web,
                    variant=variant, service_template=self.service, packager_digest="fixture")
        args.update(overrides)
        return r1.make_variant(**args)

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

    def test_archive_file_directory_collision(self):
        for records in ([('a', b'x'), ('a/b', b'x')], [('a', b'x'), ('a/b/', b'')]):
            with self.assertRaises(ValueError):
                self.read_zip(records)

    def test_duplicate_rejected(self):
        with self.assertWarns(UserWarning), self.assertRaises(ValueError):
            self.read_zip([("same", b"a"), ("same", b"b")])

    def test_archive_limits_before_decompression(self):
        with patch.object(r1, "MAX_FILE", 3), self.assertRaises(ValueError):
            self.read_zip([("large", b"1234")])
        with patch.object(r1, "MAX_TOTAL", 3), self.assertRaises(ValueError):
            self.read_zip([("a", b"12"), ("b", b"34")])
        with patch.object(r1, "MAX_ENTRIES", 1), self.assertRaises(ValueError):
            self.read_zip([("a", b"1"), ("b", b"2")])

    def test_elf_header_checks_and_android_boundary(self):
        r1.validate_helper(elf())
        for at, value in ((4, 1), (5, 2), (18, 62), (16, 1), (52, 0)):
            data = bytearray(elf())
            data[at] = value
            with self.subTest(at=at), self.assertRaises(ValueError):
                r1.validate_helper(bytes(data))
        with self.assertRaises(ValueError):
            r1.validate_helper(b"short")

    def test_unknown_base_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "unknown.zip"
            path.write_bytes(b"untrusted")
            with self.assertRaisesRegex(ValueError, "Unknown base ZIP"):
                r1.load_base(path)

    def test_pinned_digest_and_layout_with_synthetic_base(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fixture.zip"
            r1.write_zip(path, self.base)
            with patch.object(r1, "BASE_SHA256", r1.file_digest(path)):
                self.assertEqual(r1.load_base(path), self.base)
            other = Path(directory) / "missing-core.zip"
            r1.write_zip(other, {"LICENSE": (b"license", 0o644)})
            with patch.object(r1, "BASE_SHA256", r1.file_digest(other)):
                with self.assertRaisesRegex(ValueError, "unsupported module layout"):
                    r1.load_base(other)

    def test_checksums_replaced_for_every_payload(self):
        _, entries, _ = self.variant()
        self.assertNotIn("webroot/old.js", entries)
        self.assertNotIn("webroot/old.js.sha256", entries)
        for name, (data, _) in entries.items():
            if not name.endswith(".sha256"):
                self.assertEqual(entries[name + ".sha256"][0], hashlib.sha256(data).hexdigest().encode())
        self.assertFalse(any(n.endswith(".sha256.sha256") for n in entries))

    def test_three_variants_metadata(self):
        for variant in r1.VARIANTS:
            with self.subTest(variant=variant):
                name, entries, manifest = self.variant(variant)
                self.assertRegex(name, rf"^OhMyKeymint-R1-1\.3\.8-301-{variant}-[0-9a-f]{{7}}\.zip$")
                props = dict(line.split("=", 1) for line in entries["module.prop"][0].decode().splitlines())
                self.assertEqual(props["name"], name[:-4])
                self.assertEqual(props["author"], "\u6d45\u7b11\u5450")
                self.assertEqual(props["versionCode"], "301")
                self.assertEqual(props["version"], f"1.3.8-301-{variant}")
                self.assertIn("\u5f85\u542f\u52a8\u68c0\u67e5", props["description"])
                self.assertNotIn("updateJson", props)
                self.assertEqual(manifest["version"], props["version"])
                self.assertEqual(manifest["variant"], variant)
                self.assertEqual(manifest["upstream_commit"], r1.UPSTREAM_COMMIT)
                self.assertEqual(manifest["upstream_version"], "1.4.2")
                self.assertFalse(manifest["native_rebuilt"])
                self.assertEqual(json.loads(entries["r1-build.json"][0]), manifest)

    def test_last_seven_hash_and_determinism(self):
        name, entries, manifest = self.variant()
        self.assertEqual(manifest["hash"], manifest["content_sha256"][-7:])
        self.assertTrue(name.endswith(manifest["hash"] + ".zip"))
        self.assertEqual((name, entries, manifest), self.variant(base=dict(reversed(list(self.base.items())))))
        modified = dict(self.web, **{"webroot/new.js": (b"new", 0o644)})
        self.assertNotEqual(manifest["hash"], self.variant(webroot=modified)[2]["hash"])
        self.assertNotEqual(manifest["hash"], self.variant(helper=elf() + b"new")[2]["hash"])
        self.assertNotEqual(manifest["hash"], self.variant(packager_digest="changed")[2]["hash"])

    def test_recompute_documented_content_hash(self):
        _, entries, manifest = self.variant()
        snapshot = {n: e for n, e in entries.items() if not n.endswith(".sha256") and n != "r1-build.json"}
        snapshot["module.prop"] = (r1.properties(self.base["module.prop"][0],
            "OhMyKeymint-R1-1.3.8-301-full-{content_hash}", "full"), 0o644)
        context = {key: manifest[key] for key in ("base_zip_sha256", "upstream_commit", "upstream_version",
                  "variant", "packager_sha256", "service_template_sha256")}
        self.assertEqual(r1.content_digest(snapshot, context), manifest["content_sha256"])

    def test_copyright_and_native_preservation(self):
        for variant in r1.VARIANTS:
            _, entries, manifest = self.variant(variant)
            for name, original in self.base.items():
                if r1.is_license(name) or original[0].startswith(b"\x7fELF"):
                    self.assertEqual(entries[name], original)
            self.assertEqual(manifest["upstream_author"], "James Clef,ITxiao6666")
            for name, digest in manifest["upstream_native_sha256"].items():
                self.assertEqual(digest, r1.sha256(entries[name][0]))

    def test_soter_policy_and_boot_order(self):
        for variant in r1.VARIANTS:
            _, entries, manifest = self.variant(variant)
            self.assertEqual("r1-no-soter" in entries, variant != "full")
            self.assertEqual("soter/standalone.conf" in entries, variant == "full")
            self.assertIn("libs/arm64-v8a/libsoter.so", entries)
            self.assertTrue(manifest["soter_code_in_upstream_binaries"])
            service = entries["service.sh"][0]
            self.assertLess(service.index(b"--webui-set-soter-local-base64"), service.index(b"start_daemon"))
            self.assertIn(b"exit 1", service)
            self.assertIn(b'"$R1_HELPER" monitor &', service)

    def test_native_disable_changes_only_enabled(self):
        payload = self.service.split(b"--webui-set-soter-local-base64", 1)[1].split()[1]
        self.assertEqual(json.loads(base64.b64decode(payload)), {"action": "enabled", "enabled": False})
        self.assertIn(b'if [ -f "$MODDIR/r1-build.json" ]; then', self.service)
        self.assertIn(b'if [ -f "$MODDIR/r1-no-soter" ]; then', self.service)
        self.assertIn(b"LD_LIBRARY_PATH=", self.service)

    def test_install_manifests_and_helper_permissions(self):
        for variant in r1.VARIANTS:
            _, entries, _ = self.variant(variant)
            for manifest in ("webroot.manifest", "r1-extra.manifest"):
                for name in entries[manifest][0].decode().splitlines():
                    self.assertIn(name, entries)
                    self.assertIn(name + ".sha256", entries)
            self.assertEqual(entries[r1.HELPER_PATH][1], 0o755)
            self.assertIn(b'chmod 0755 "$MODPATH/arm64-v8a" "$MODPATH/arm64-v8a/r1-helper"', entries["customize.sh"][0])
            self.assertIn(b'extract "$ZIPFILE" \'r1-extra.manifest\' "$MODPATH"', entries["customize.sh"][0])
            self.assertEqual(entries["verify.sh"], self.base["verify.sh"])

    def test_unsupported_script_anchor_rejected(self):
        base = dict(self.base, **{"customize.sh": (b"unknown installer", 0o755)})
        with self.assertRaisesRegex(ValueError, "Unsupported upstream script anchor"):
            self.variant(base=base)

    def test_low_disk_entry_and_stage_gates(self):
        for free, entry in ((6 * r1.GIB - 1, True), (2 * r1.GIB - 1, False)):
            with patch.object(shutil, "disk_usage", return_value=shutil._ntuple_diskusage(20 * r1.GIB, 0, free)):
                with self.assertRaisesRegex(RuntimeError, "Paused"):
                    r1.check_disk(ROOT, entry=entry)
        with patch.object(shutil, "disk_usage", return_value=shutil._ntuple_diskusage(20 * r1.GIB, 0, 6 * r1.GIB)):
            r1.check_disk(ROOT, entry=True)

    def test_existing_output_refused_before_loading_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            with patch.object(r1, "load_base") as load, self.assertRaises(FileExistsError):
                r1.package(path / "base.zip", path / "helper", path / "web", path)
            load.assert_not_called()

    def test_synthetic_zip_reproducible_and_exclusive(self):
        with tempfile.TemporaryDirectory() as directory:
            first, second = Path(directory) / "first.zip", Path(directory) / "second.zip"
            r1.write_zip(first, {"fixture": (b"data", 0o755)})
            r1.write_zip(second, {"fixture": (b"data", 0o755)})
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with self.assertRaises(FileExistsError):
                r1.write_zip(first, {})
            with zipfile.ZipFile(first) as archive:
                self.assertEqual(archive.getinfo("fixture").external_attr >> 16 & 0o777, 0o755)

    def test_webroot_symlink_and_incomplete_build_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            with self.assertRaisesRegex(ValueError, "Incomplete"):
                r1.load_webroot(path)
            (path / "index.html").write_bytes(b"index")
            (path / "config.json").write_bytes(b"{}")
            (path / "stale.sha256").write_bytes(b"old")
            self.assertNotIn("webroot/stale.sha256", r1.load_webroot(path))
            (path / "link").symlink_to(path / "index.html")
            with self.assertRaisesRegex(ValueError, "symlink"):
                r1.load_webroot(path)

    def test_webroot_enumeration_is_bounded(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "a").mkdir()
            (path / "b").mkdir()
            with patch.object(r1, "MAX_ENTRIES", 1), self.assertRaisesRegex(ValueError, "count limit"):
                r1.load_webroot(path)


if __name__ == "__main__":
    unittest.main()
