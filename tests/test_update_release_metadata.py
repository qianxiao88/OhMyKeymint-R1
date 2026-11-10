"""Release tooling checks using synthetic ZIPs; no builds or network access."""

import importlib.util
import json
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from urllib.parse import quote
import warnings
import zipfile


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts/update_release_metadata.py"
SPEC = importlib.util.spec_from_file_location("update_release_metadata", SCRIPT)
metadata = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(metadata)
VARIANTS = ("lite", "no-soter", "full")


class MetadataTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.output = self.directory / "output"
        self.release = {"tag_name": "R1/v3", "body": "\u6d45\u7b11\u5450\r\nnotes\r\n",
                        "draft": False, "prerelease": False, "assets": []}
        for index, variant in enumerate(VARIANTS):
            name = f"OhMyKeymint-R1-1.3.8-301-{variant}-abc1234.zip"
            self.release["assets"].append({
                "name": name, "state": "uploaded",
                "browser_download_url": (
                    f"https://github.com/{metadata.R1_REPO}/releases/download/"
                    f"{quote(self.release['tag_name'], safe='')}/{name}"),
            })
            self.write_zip(index, self.properties(index))

    def properties(self, index):
        variant = VARIANTS[index]
        return (f"# module properties\r\nid=oh_my_keymint\r\n"
                f"name={self.release['assets'][index]['name'][:-4]}\r\n"
                f"version=1.3.8-301-{variant}\r\nversionCode={301 + index}\r\n"
                f"updateJson={metadata.R1_RAW}/update-{variant}.json\r\n").encode()

    def write_zip(self, index, data, entry="module.prop", mode=None, duplicate=False):
        path = self.directory / self.release["assets"][index]["name"]
        info = zipfile.ZipInfo(entry)
        if mode is not None:
            info.create_system = 3
            info.external_attr = mode << 16
        with zipfile.ZipFile(path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.writestr(info, data)
            if duplicate:
                with warnings.catch_warnings():
                    warnings.simplefilter("ignore", UserWarning)
                    archive.writestr(info, data)

    def run_main(self, *options):
        release_json = self.directory / "release.json"
        release_json.write_text(json.dumps(self.release), encoding="utf-8")
        with patch.object(sys, "argv", [str(SCRIPT), "--release-json", str(release_json),
                                       "--output-dir", str(self.output), *options]):
            metadata.main()

    def test_r1_actual_codes_urls_and_utf8(self):
        # Unrelated upstream/debug ZIPs and sidecars must not be consumed by R1.
        self.release["assets"].extend([
            {"name": "upstream-release.zip", "state": "uploaded"},
            {"name": "debug.zip", "state": "uploaded"},
            {"name": "SHA256SUMS", "state": "uploaded"},
        ])
        self.run_main("--r1", "--asset-dir", str(self.directory), "--version-code", "999")
        self.assertEqual({p.name for p in self.output.iterdir()},
                         {"changelog.md", *(f"update-{v}.json" for v in VARIANTS)})
        self.assertEqual((self.output / "changelog.md").read_bytes(),
                         "\u6d45\u7b11\u5450\nnotes\n".encode())
        for index, variant in enumerate(VARIANTS):
            data = (self.output / f"update-{variant}.json").read_bytes()
            self.assertTrue(data.endswith(b"\n"))
            self.assertNotIn(b"\r", data)
            self.assertEqual(json.loads(data), {
                "version": f"1.3.8-301-{variant}", "versionCode": 301 + index,
                "zipUrl": self.release["assets"][index]["browser_download_url"],
                "changelog": f"{metadata.R1_RAW}/changelog.md",
            })

    def test_upstream_contract(self):
        self.release["assets"] = [
            {"name": "upstream-release.zip", "state": "uploaded",
             "browser_download_url": "https://example.org/upstream.zip"},
            {"name": "upstream-debug.zip", "state": "uploaded"},
        ]
        self.run_main("--version-code", "876")
        self.assertEqual(json.loads((self.output / "update.json").read_bytes()), {
            "version": "R1/v3", "versionCode": 876,
            "zipUrl": "https://example.org/upstream.zip",
            "changelog": "https://raw.githubusercontent.com/ITxiao6666/OhMyKeymint/changelog/changelog.md",
        })
        self.assertEqual({p.name for p in self.output.iterdir()}, {"update.json", "changelog.md"})

    def test_old_release_edit_preserves_all_current_metadata(self):
        self.run_main("--r1", "--asset-dir", str(self.directory))
        newer = self.output / "update-full.json"
        update = json.loads(newer.read_bytes())
        update["versionCode"] = 304
        newer.write_text(json.dumps(update), encoding="utf-8")
        before = {p.name: p.read_bytes() for p in self.output.iterdir()}
        self.release["body"] = "edited old release notes"
        self.run_main("--r1", "--asset-dir", str(self.directory))
        self.assertEqual({p.name: p.read_bytes() for p in self.output.iterdir()}, before)

    def test_draft_and_prerelease_skip_without_assets(self):
        self.release["assets"] = []
        for flag in ("draft", "prerelease"):
            self.release[flag] = True
            self.run_main("--r1")
            self.run_main()
            self.assertFalse(self.output.exists())
            self.release[flag] = False

    def test_invalid_properties_leave_output_untouched(self):
        original = self.properties(2)
        invalid = [original.replace(b"versionCode=303", f"versionCode={value}".encode())
                   for value in ("0", "-1", "1.5", "abc", "2147483648", "$(id)", "")]
        invalid.extend([
            original.replace(b"id=oh_my_keymint", b"id=other"),
            original.replace(b"version=1.3.8-301-full", b"version=1.3.8-301-lite"),
            original.replace(b"update-full.json", b"update-lite.json"),
            original.replace(b"name=OhMyKeymint", b"name=Other"),
            original + b"versionCode=303\n", b"\xff", b"x" * 65537,
            original + b"invalid line\n",
        ])
        self.output.mkdir()
        sentinel = self.output / "update-lite.json"
        sentinel.write_bytes(b"existing metadata")
        for data in invalid:
            with self.subTest(data=data[:80]):
                self.write_zip(2, data)
                with self.assertRaises((ValueError, UnicodeDecodeError)):
                    self.run_main("--r1", "--asset-dir", str(self.directory))
                self.assertEqual(sentinel.read_bytes(), b"existing metadata")
                self.assertEqual(list(self.output.iterdir()), [sentinel])

    def test_missing_duplicate_or_incomplete_variants(self):
        original = list(self.release["assets"])
        cases = [original[:2], original + [original[0]],
                 [dict(original[0], state="new"), *original[1:]],
                 [dict(original[0], name="OhMyKeymint-R1-unknown.zip"), *original[1:]],
                 [dict(original[0], browser_download_url="https://example.org/wrong.zip"),
                  *original[1:]]]
        for assets in cases:
            with self.subTest(assets=assets):
                self.release["assets"] = assets
                with self.assertRaises(ValueError):
                    self.run_main("--r1", "--asset-dir", str(self.directory))
                self.assertFalse(self.output.exists())

    def test_module_prop_must_be_single_bounded_regular_root_file(self):
        for options in ({"entry": "nested/module.prop"}, {"entry": "../module.prop"},
                        {"mode": stat.S_IFLNK | 0o777}, {"duplicate": True}):
            with self.subTest(options=options):
                self.write_zip(0, self.properties(0), **options)
                with self.assertRaises(ValueError):
                    self.run_main("--r1", "--asset-dir", str(self.directory))
                self.assertFalse(self.output.exists())

    def test_invalid_zip_and_missing_download(self):
        path = self.directory / self.release["assets"][0]["name"]
        path.write_bytes(b"not a ZIP")
        with self.assertRaises(zipfile.BadZipFile):
            self.run_main("--r1", "--asset-dir", str(self.directory))
        with self.assertRaises(ValueError):
            metadata.r1_updates(self.release, self.directory / "missing")
        self.assertFalse(self.output.exists())

    def test_required_arguments_and_upstream_asset_count(self):
        with self.assertRaises(SystemExit), patch("sys.stderr"):
            self.run_main("--r1")
        for code in ((), ("--version-code", "0")):
            with self.assertRaises(ValueError):
                self.run_main(*code)
        with self.assertRaisesRegex(ValueError, "exactly one"):
            self.run_main("--version-code", "301")

    def test_workflow_shell_syntax_and_routing(self):
        workflow = (ROOT / ".github/workflows/update-changelog.yml").read_text(encoding="utf-8")
        self.assertIn("!github.event.release.draft && !github.event.release.prerelease", workflow)
        self.assertEqual(workflow.count("if: steps.release_info.outputs.mode == 'upstream'"), 2)
        self.assertIn("if: steps.release_info.outputs.mode == 'r1'", workflow)
        self.assertIn('[ "$r1_count" -eq 0 ] || [ "$r1_count" -eq 3 ]', workflow)
        self.assertIn('git push origin "HEAD:master"', workflow)
        self.assertIn('git pull --ff-only origin master', workflow)
        self.assertIn('git pull --rebase origin master', workflow)
        self.assertIn('group: release-update-metadata', workflow)
        self.assertIn('git push origin "HEAD:changelog"', workflow)
        self.assertEqual(workflow.count('git config user.name "\u6d45\u7b11\u5450"'), 2)
        self.assertEqual(workflow.count('git config user.email "2082387009@qq.com"'), 2)
        for block in workflow.split("        run: |\n")[1:]:
            lines = []
            for line in block.splitlines():
                if line and not line.startswith("          "):
                    break
                lines.append(line[10:])
            result = subprocess.run(["bash", "-n"], input="\n".join(lines),
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
