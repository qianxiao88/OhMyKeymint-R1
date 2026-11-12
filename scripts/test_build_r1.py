"""Build orchestration tests with fake artifacts; never launch Cargo or Vite."""

import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("r1_build", ROOT / "build.py")
build = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build)


class BuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.target = self.root / "cargo-target"
        self.target.mkdir()
        self.env = {"CARGO_TARGET_DIR": str(self.target)}

    def artifact(self, name, data=b"native"):
        path = self.target / "aarch64-linux-android/release" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def test_feature_commands_for_each_native_variant(self):
        for variant in build.r1.VARIANTS:
            for package, binary in ((None, "keymint"), ("injector", "inject")):
                with self.subTest(variant=variant, package=package):
                    artifact = self.artifact(binary)
                    with patch.object(build, "cargo_context_for_target", return_value=(self.env, "patch")), \
                            patch.object(build, "run") as run:
                        result = build.build_binary(abi="arm64-v8a", target="aarch64-linux-android",
                                                    release=True, package=package, bin_name=binary,
                                                    variant=variant)
                    self.assertEqual(result, artifact)
                    command = run.call_args.args[0]
                    self.assertEqual(command[:7], ["cargo", "+nightly", "--config", "patch", "build",
                                                   "--target", "aarch64-linux-android"])
                    self.assertIn("--release", command)
                    self.assertEqual("--no-default-features" in command, variant != "full")
                    expected = ["pif,local-rkp"] if variant == "no-soter" and package is None else []
                    actual = [command[command.index("--features") + 1]] if "--features" in command else []
                    self.assertEqual(actual, expected)

    def test_target_dir_respects_absolute_relative_and_helper_workspace(self):
        self.assertEqual(build.cargo_target_root(env=self.env), self.target)
        self.assertEqual(build.cargo_target_root(env={"CARGO_TARGET_DIR": "custom-target"}),
                         (build.REPO_ROOT / "custom-target").resolve())
        self.assertEqual(build.cargo_target_root(env={}), build.REPO_ROOT / "target")
        self.assertEqual(build.cargo_target_root(manifest=build.HELPER_MANIFEST, env={}),
                         build.HELPER_MANIFEST.parent / "target")

    def test_helper_build_uses_independent_manifest_and_isolated_copy(self):
        helper = self.artifact("r1-helper", b"helper-source-output")
        stage = self.root / "stage"
        stage.mkdir()
        with patch.dict(os.environ, self.env), patch.object(build, "run") as run:
            build.build_helper(target="aarch64-linux-android", release=True, stage_dir=stage)
        command = run.call_args.args[0]
        self.assertEqual(command[command.index("--manifest-path") + 1], str(build.HELPER_MANIFEST))
        self.assertEqual(command[command.index("--target") + 1], "aarch64-linux-android")
        self.assertEqual(command[command.index("--bin") + 1], "r1-helper")
        helper.write_bytes(b"later-overwrite")
        self.assertEqual((stage / build.r1.HELPER_PATH).read_bytes(), b"helper-source-output")

    def test_skroot_build_compiles_each_variant_with_matching_manifest_version(self):
        loader = self.artifact("libomk_skroot_loader.so", b"loader")
        worker = self.artifact("omk-skroot-worker", b"worker")
        manifest = self.root / "skroot-workspace/Cargo.toml"
        manifest.parent.mkdir()
        manifest.write_text("[workspace]\n")
        for variant in build.r1.VARIANTS:
            stage = self.root / ("stage-" + variant)
            stage.mkdir()
            with patch.dict(os.environ, self.env), patch.object(build, "SKROOT_MANIFEST", manifest), \
                    patch.object(build, "run") as run:
                output_loader, output_worker = build.build_skroot_loader(
                    target="aarch64-linux-android", release=True, variant=variant, stage_dir=stage)
            command = run.call_args.args[0]
            self.assertEqual(command[command.index("--manifest-path") + 1], str(manifest))
            self.assertIn("--lib", command)
            self.assertEqual(command[command.index("--bin") + 1], "omk-skroot-worker")
            self.assertEqual(run.call_args.kwargs["env"]["OMK_SKROOT_VERSION"], f"1.3.8-302-{variant}")
            self.assertEqual(run.call_args.kwargs["env"]["OMK_SKROOT_VARIANT"], variant)
            self.assertEqual(output_loader.read_bytes(), loader.read_bytes())
            self.assertEqual(output_worker.read_bytes(), worker.read_bytes())
            self.assertNotEqual(output_loader, loader)
            self.assertNotEqual(output_worker, worker)

    def test_staged_variant_policy_and_service_keep_only_selected_features(self):
        policy_source = (ROOT / "template/sepolicy.rule").read_bytes()
        service_source = (ROOT / "template/service.sh").read_bytes()
        soter_start = policy_source.index(b"# Native SoterServer")
        soter_end = policy_source.index(b"# Permit only the verified EngineerMode")
        soter_block = policy_source[soter_start:soter_end]
        for variant in build.r1.VARIANTS:
            stage = self.root / ("policy-" + variant)
            stage.mkdir()
            (stage / "sepolicy.rule").write_bytes(policy_source)
            (stage / "service.sh").write_bytes(service_source)
            build.configure_template_for_variant(stage, variant)
            policy = (stage / "sepolicy.rule").read_bytes()
            service = (stage / "service.sh").read_bytes()
            if variant == "full":
                self.assertEqual(policy, policy_source)
                self.assertEqual(service, service_source)
            elif variant == "no-soter":
                self.assertEqual(policy, policy_source.replace(soter_block, b"", 1))
                self.assertEqual(service, service_source)
            else:
                self.assertNotIn(b"SoterServer", policy)
                self.assertNotIn(b"engineermode_app", policy)
                self.assertNotIn(b"EngineerMode", policy)
                self.assertNotIn(b"exact UID-1000", policy)
                self.assertNotIn(b"Ommega 9583e028", policy)
                for required in (b"allow keystore system_file", b"allow system_file keystore",
                                 b"allow crash_dump keystore process *", b"service_manager find"):
                    self.assertIn(required, policy)
                self.assertEqual(service, service_source.split(b"# Legacy property adjustments", 1)[0].rstrip() + b"\n")
                for required in (b'"$R1_HELPER" monitor &',
                                 b'start_daemon "$MODDIR/daemon" "$STATE_DIR/keymint-daemon.pid"',
                                 b'start_daemon "$MODDIR/daemon-injector" "$STATE_DIR/injector-daemon.pid"'):
                    self.assertIn(required, service)
                for removed in (b"Legacy property adjustments", b"persist.logd", b"sys.usb.adb", b"ro.debuggable", b"ro.kernel.qemu"):
                    self.assertNotIn(removed, service)
        self.assertEqual((ROOT / "template/sepolicy.rule").read_bytes(), policy_source)
        self.assertEqual((ROOT / "template/service.sh").read_bytes(), service_source)

    def test_webui_build_selects_matching_variant_and_placeholder_identity(self):
        stage = self.root / "stage"
        for variant in build.r1.VARIANTS:
            with patch.object(build, "run") as run, patch.object(build.r1, "load_webroot"):
                build.build_webui(stage, variant)
            self.assertEqual(run.call_args.args[0], ["pnpm", "--dir", "webui", "build"])
            env = run.call_args.kwargs["env"]
            self.assertEqual(env["VITE_R1_VARIANT"], variant)
            self.assertEqual(env["VITE_R1_NAME"], build.r1.module_name(variant))
            self.assertEqual(env["VITE_R1_OUT_DIR"], str((stage / "webroot").resolve()))

    def test_engineer_lookup_stays_in_repo_target_not_cargo_target(self):
        local_target = self.root / "repository-target"
        bridge = local_target / "engineer-bridge/aarch64-linux-android"
        (bridge / "native").mkdir(parents=True)
        (bridge / "licenses").mkdir()
        (bridge / "native/libomk_engineer.so").write_bytes(b"engineer")
        (bridge / "classes.dex").write_bytes(b"dex")
        (bridge / "licenses/LSPlant.txt").write_bytes(b"license")
        payload = self.artifact("libpif_spoof.so", b"pif")
        stage = self.root / "stage"
        with patch.dict(os.environ, self.env), patch.object(build, "TARGET_ROOT", local_target):
            build.copy_zygisk_payload(payload, "arm64-v8a", stage)
        self.assertEqual((stage / "zygisk/arm64-v8a.so").read_bytes(), b"pif")
        self.assertEqual((stage / "libs/arm64-v8a/libomk_engineer.so").read_bytes(), b"engineer")
        self.assertEqual((stage / "libs/arm64-v8a/omk_engineer.dex").read_bytes(), b"dex")
        self.assertEqual((stage / "THIRD_PARTY_LICENSES/engineer/LSPlant.txt").read_bytes(), b"license")

    def test_variants_copy_each_native_before_next_build_and_skip_lite_payload(self):
        shared = self.artifact("shared")
        observations = []
        events = []

        def compile_binary(**kwargs):
            events.append("build-" + kwargs["bin_name"])
            shared.write_bytes((kwargs["variant"] + "-" + kwargs["bin_name"]).encode())
            return shared

        original_copy = build.copy_binary

        def copy_binary(binary, name, abi, stage):
            events.append("copy-" + name)
            original_copy(binary, name, abi, stage)

        def finalize(stage, variant, **kwargs):
            observations.append((variant, (stage / "libs/arm64-v8a/keymint").read_bytes(),
                                 (stage / "libs/arm64-v8a/inject").read_bytes()))
            return f"fixture-{variant}.zip", {"fixture": (variant.encode(), 0o644)}, {}

        output = self.root / "output"
        output.mkdir()
        for variant in build.r1.VARIANTS:
            with patch.dict(os.environ, self.env), \
                    patch.object(build, "copy_template_files"), patch.object(build, "copy_project_documents"), \
                    patch.object(build, "normalize_module_text_files"), patch.object(build, "configure_template_for_abi"), \
                    patch.object(build, "configure_template_for_variant") as configure_variant, \
                    patch.object(build, "build_binary", side_effect=compile_binary), \
                    patch.object(build, "copy_binary", side_effect=copy_binary), patch.object(build, "build_helper"), \
                    patch.object(build, "build_engineer_bridge") as bridge, \
                    patch.object(build, "build_cdylib", return_value=shared) as payload, \
                    patch.object(build, "copy_zygisk_payload") as copy_payload, \
                    patch.object(build, "build_webui"), patch.object(build, "builder_digests", return_value={}), \
                    patch.object(build.r1, "check_disk"), \
                    patch.object(build.r1, "finalize_variant", side_effect=finalize), \
                    patch.object(build, "build_skroot_loader", return_value=(shared, shared)), \
                    patch.object(build.r1, "finalize_skroot_variant", \
                                 return_value=(f"fixture-{variant}.zip", {"fixture": (b"skroot", 0o644)}, {})):
                result = build.build_package_for_abi(
                    abi="arm64-v8a", release=True, platform=26, variant=variant, output=output,
                    source={"head": "a" * 40, "dirty": True}, upstream_version="1.4.2")
            self.assertTrue(result.is_file())
            self.assertEqual(configure_variant.call_count, 1)
            self.assertEqual(configure_variant.call_args.args[1], variant)
            self.assertTrue(result.with_suffix(".zip.sha256").is_file())
            self.assertTrue((output / "skrootpro" / result.name).is_file())
            self.assertTrue((output / "skrootpro" / (result.name + ".sha256")).is_file())
            self.assertEqual(bridge.call_count, int(variant != "lite"))
            self.assertEqual(payload.call_count, int(variant != "lite"))
            self.assertEqual(copy_payload.call_count, int(variant != "lite"))
        self.assertEqual(events, ["build-keymint", "copy-keymint", "build-inject", "copy-inject"] * 3)
        self.assertEqual(observations, [(variant, f"{variant}-keymint".encode(), f"{variant}-inject".encode())
                                        for variant in build.r1.VARIANTS])
        self.assertFalse(list(self.target.glob("r1-*")))

    def test_zip_sidecar_reuses_matching_checksum_and_keeps_conflict(self):
        package = self.root / "release.zip"
        package.write_bytes(b"package")
        build.write_zip_sidecar(package)
        sidecar = package.with_suffix(".zip.sha256")
        expected = sidecar.read_bytes()
        timestamp = sidecar.stat().st_mtime_ns
        build.write_zip_sidecar(package)
        self.assertEqual(sidecar.read_bytes(), expected)
        self.assertEqual(sidecar.stat().st_mtime_ns, timestamp)
        sidecar.write_bytes(b"user checksum")
        with self.assertRaisesRegex(FileExistsError, "checksum differs"):
            build.write_zip_sidecar(package)
        self.assertEqual(sidecar.read_bytes(), b"user checksum")

    def test_main_all_preserves_user_zips_and_uses_fixed_code302(self):
        output = self.root / "output"
        output.mkdir()
        previous = output / "OhMyKeymint-user-release.zip"
        previous.write_bytes(b"user archive")
        with patch.object(build.r1, "check_disk"), patch.object(build, "get_source_info", return_value={"head": "a" * 40, "dirty": True}), \
                patch.object(build, "get_version_from_cargo_toml", return_value="1.4.2"), \
                patch.object(build, "build_package_for_abi", return_value=output / "new.zip") as package:
            status = build.main(["--release", "--variant", "all", "--output", str(output)])
        self.assertEqual(status, 0)
        self.assertEqual([call.kwargs["variant"] for call in package.call_args_list], list(build.r1.VARIANTS))
        self.assertEqual(previous.read_bytes(), b"user archive")
        self.assertEqual(build.r1.VERSION_CODE, 302)
        self.assertFalse(hasattr(build, "delete_old_zips"))


if __name__ == "__main__":
    unittest.main()
