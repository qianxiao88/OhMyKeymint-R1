#!/usr/bin/env python3
"""Build and package feature-selected R1 Android modules from source."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

try:
    import tomllib as toml
except ModuleNotFoundError:
    import toml

from scripts import package_r1 as r1


REPO_ROOT = Path(__file__).resolve().parent
# Engineer's Rust build script and Python builder intentionally share this path.
TARGET_ROOT = REPO_ROOT / "target"
DEFAULT_PLATFORM = 24
ABI_TO_TARGET = {"arm64-v8a": "aarch64-linux-android"}
ABI_TO_MODULE_ARCHES = {"arm64-v8a": "arm64 arm64-v8a"}
BORINGSSL_BUILD_DIRS = {
    "aarch64-linux-android": Path.home() / ".cargo" / "boringssl" / "build",
}
BINARY_SPECS = (
    {"package": None, "bin": "keymint", "output_name": "keymint"},
    {"package": "injector", "bin": "inject", "output_name": "inject"},
)
PIF_SPOOF_PACKAGE = "pif-spoof"
PIF_SPOOF_LIBRARY = "libpif_spoof.so"
HELPER_MANIFEST = REPO_ROOT / "tools/r1-runtime/Cargo.toml"
SKROOT_MANIFEST = REPO_ROOT / "tools/skroot-loader/Cargo.toml"
REQUIRED_TEMPLATE_FILES = (
    "customize.sh", "daemon", "daemon-injector", "injector.toml", "module.prop",
    "uninstall.sh", "post-fs-data.sh", "service.sh", "verify.sh",
)
MODULE_TEXT_FILES = (
    "AOSP.Apache-license-2.0.txt", "LICENSE-2", "LICENSE.md", "README.md",
    "THIRD_PARTY_LICENSES/Tricky-Addon-Update-Target-List.txt",
    "THIRD_PARTY_LICENSES/zygisk-api-0BSD.txt", "customize.sh", "daemon", "daemon-injector",
    "injector.toml", "keybox.xml", "google_attestation_status.json", "module.prop",
    "uninstall.sh", "post-fs-data.sh", "sepolicy.rule", "service.sh", "verify.sh",
    "META-INF/com/google/android/update-binary", "META-INF/com/google/android/updater-script",
)


def run(cmd: list[str], *, env: dict[str, str] | None = None) -> None:
    print("+", " ".join(cmd), flush=True)
    result = subprocess.run(cmd, cwd=REPO_ROOT, env=env)
    if result.returncode != 0:
        raise RuntimeError(f"command failed ({result.returncode}): {' '.join(cmd)}")


def get_version_from_cargo_toml() -> str:
    with (REPO_ROOT / "Cargo.toml").open("r", encoding="utf-8") as stream:
        return toml.loads(stream.read())["package"]["version"]


def get_source_info() -> dict:
    def git(*args: str) -> str:
        result = subprocess.run(
            ["git", "-c", f"safe.directory={REPO_ROOT}", *args], cwd=REPO_ROOT,
            capture_output=True, text=True,
        )
        if result.returncode != 0:
            raise RuntimeError(f"Failed to inspect source: {result.stderr.strip()}")
        return result.stdout.strip()

    return {"head": git("rev-parse", "HEAD"),
            "dirty": bool(git("status", "--porcelain", "--untracked-files=normal"))}


def cargo_target_root(*, manifest: Path | None = None, env: dict[str, str] | None = None) -> Path:
    configured = (os.environ if env is None else env).get("CARGO_TARGET_DIR")
    if configured:
        path = Path(configured).expanduser()
        return path.resolve() if path.is_absolute() else (REPO_ROOT / path).resolve()
    return (manifest.parent if manifest else REPO_ROOT) / "target"


def cargo_context_for_target(target: str) -> tuple[dict[str, str], str]:
    env = os.environ.copy()
    configured_dir = env.get("BORINGSSL_BUILD_DIR")
    if configured_dir:
        build_dir = Path(configured_dir).expanduser()
        if not build_dir.is_absolute():
            build_dir = REPO_ROOT / build_dir
    else:
        build_dir = BORINGSSL_BUILD_DIRS.get(target)
        if build_dir is None:
            raise ValueError(f"No default BoringSSL build directory for target {target}")
    build_dir = build_dir.resolve()
    bssl_sys_dir = build_dir.parent / "rust" / "bssl-sys"
    required_paths = (
        bssl_sys_dir / "Cargo.toml", build_dir / "libcrypto.a", build_dir / "libssl.a",
        build_dir / "rust/bssl-sys/librust_wrapper.a",
        build_dir / "rust/bssl-sys" / f"wrapper_{target}.rs",
    )
    missing = [path for path in required_paths if not path.is_file()]
    if missing:
        raise FileNotFoundError(
            f"BoringSSL is not built for {target}; missing: "
            + ", ".join(os.fspath(path) for path in missing)
            + f". Build it with -DRUST_BINDINGS={target}, or set BORINGSSL_BUILD_DIR."
        )
    env["BORINGSSL_BUILD_DIR"] = os.fspath(build_dir)
    if env.get("CARGO_TARGET_DIR"):
        env["CARGO_TARGET_DIR"] = os.fspath(cargo_target_root(env=env))
    cargo_patch = f"patch.crates-io.bssl-sys.path={json.dumps(bssl_sys_dir.as_posix())}"
    return env, cargo_patch


def feature_args(variant: str, package: str | None) -> list[str]:
    if variant not in r1.VARIANTS:
        raise ValueError(f"Unsupported variant: {variant}")
    if variant == "full":
        return []
    if package == "injector" or variant == "lite":
        return ["--no-default-features"]
    return ["--no-default-features", "--features", ",".join(r1.FEATURES[variant])]


def build_binary(*, abi: str, target: str, release: bool, package: str | None,
                 bin_name: str, variant: str) -> Path:
    build_type = "release" if release else "debug"
    print(f"Building {bin_name}: {variant}, {abi}, {build_type}", flush=True)
    env, cargo_patch = cargo_context_for_target(target)
    cmd = ["cargo", "+nightly", "--config", cargo_patch, "build", "--target", target]
    if package:
        cmd.extend(["-p", package])
    cmd.extend(["--bin", bin_name, *feature_args(variant, package)])
    if release:
        cmd.append("--release")
    run(cmd, env=env)
    binary_path = cargo_target_root(env=env) / target / build_type / bin_name
    if not binary_path.is_file():
        raise FileNotFoundError(f"Built binary not found at {binary_path}")
    return binary_path


def copy_binary(binary: Path, output_name: str, abi: str, stage_dir: Path) -> None:
    destination = stage_dir / "libs" / abi / output_name
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(binary, destination)
    print(f"Isolated {binary} -> {destination}", flush=True)


def build_cdylib(*, abi: str, target: str, release: bool, package: str,
                 library_name: str) -> Path:
    build_type = "release" if release else "debug"
    print(f"Building {package} cdylib: {abi}, {build_type}", flush=True)
    env, cargo_patch = cargo_context_for_target(target)
    cmd = ["cargo", "+nightly", "--config", cargo_patch, "build", "--target", target,
           "-p", package, "--lib"]
    if release:
        cmd.append("--release")
    run(cmd, env=env)
    library = cargo_target_root(env=env) / target / build_type / library_name
    if not library.is_file():
        raise FileNotFoundError(f"Built cdylib not found at {library}")
    return library


def build_engineer_bridge(target: str) -> None:
    sdk_value = os.environ.get("ANDROID_HOME") or os.environ.get("ANDROID_SDK_ROOT")
    if sdk_value:
        sdk = Path(sdk_value).expanduser().resolve()
    elif os.environ.get("LOCALAPPDATA"):
        sdk = Path(os.environ["LOCALAPPDATA"]) / "Android/Sdk"
    else:
        raise RuntimeError("Engineer bridge requires ANDROID_HOME or ANDROID_SDK_ROOT")
    ndk = Path(os.environ.get("ANDROID_NDK_ROOT", os.fspath(sdk / "ndk/30.0.16248370")))
    run([sys.executable, "scripts/build_engineer_bridge.py", "--target", target,
         "--sdk", os.fspath(sdk), "--ndk", os.fspath(ndk)])


def copy_zygisk_payload(library: Path, abi: str, stage_dir: Path) -> None:
    destination = stage_dir / "zygisk" / f"{abi}.so"
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(library, destination)
    # Do not redirect this bridge lookup to CARGO_TARGET_DIR: pif-spoof/build.rs
    # embeds its DEX from the repository-local engineer-bridge directory.
    bridge_root = TARGET_ROOT / "engineer-bridge" / ABI_TO_TARGET[abi]
    files = {bridge_root / "native/libomk_engineer.so": stage_dir / "libs" / abi / "libomk_engineer.so",
             bridge_root / "classes.dex": stage_dir / "libs" / abi / "omk_engineer.dex"}
    for source, destination in files.items():
        if not source.is_file():
            raise FileNotFoundError(f"Engineer bridge is missing: {source}")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
    shutil.copytree(bridge_root / "licenses", stage_dir / "THIRD_PARTY_LICENSES/engineer",
                    dirs_exist_ok=True)


def build_helper(*, target: str, release: bool, stage_dir: Path) -> None:
    env = os.environ.copy()
    if env.get("CARGO_TARGET_DIR"):
        env["CARGO_TARGET_DIR"] = os.fspath(cargo_target_root(env=env))
    cmd = ["cargo", "+nightly", "build", "--manifest-path", os.fspath(HELPER_MANIFEST),
           "--target", target, "--bin", "r1-helper"]
    if release:
        cmd.append("--release")
    run(cmd, env=env)
    helper = cargo_target_root(manifest=HELPER_MANIFEST, env=env) / target / (
        "release" if release else "debug") / "r1-helper"
    if not helper.is_file():
        raise FileNotFoundError(f"Built helper not found at {helper}")
    destination = stage_dir / r1.HELPER_PATH
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(helper, destination)


def build_skroot_loader(*, target: str, release: bool, variant: str,
                        stage_dir: Path) -> tuple[Path, Path]:
    if not SKROOT_MANIFEST.is_file():
        raise FileNotFoundError(f"SKRootPro loader workspace is missing: {SKROOT_MANIFEST}")
    env = os.environ.copy()
    env.update(OMK_SKROOT_VERSION=f"{r1.BRAND}-{variant}", OMK_SKROOT_VARIANT=variant)
    if env.get("CARGO_TARGET_DIR"):
        env["CARGO_TARGET_DIR"] = os.fspath(cargo_target_root(env=env))
    cmd = ["cargo", "+nightly", "build", "--manifest-path", os.fspath(SKROOT_MANIFEST),
           "--target", target, "--lib", "--bin", "omk-skroot-worker"]
    if release:
        cmd.append("--release")
    run(cmd, env=env)
    root = cargo_target_root(manifest=SKROOT_MANIFEST, env=env) / target / (
        "release" if release else "debug")
    destination = stage_dir / "skroot-loader"
    destination.mkdir()
    outputs = []
    for name in ("libomk_skroot_loader.so", "omk-skroot-worker"):
        source = root / name
        if not source.is_file():
            raise FileNotFoundError(f"Built SKRootPro component not found at {source}")
        output = destination / name
        shutil.copyfile(source, output)
        outputs.append(output)
    return outputs[0], outputs[1]


def copy_template_files(stage_dir: Path) -> None:
    template = REPO_ROOT / "template"
    missing = [name for name in REQUIRED_TEMPLATE_FILES if not (template / name).is_file()]
    if missing:
        raise FileNotFoundError(f"Template is missing required file(s): {', '.join(missing)}")
    # Never inherit a compiled WebUI or native binary from the template tree.
    excluded = {"webroot", "libs", "zygisk", "arm64-v8a", "r1-build.json",
                "webroot.manifest", "payload.manifest", "r1-extra.manifest", "r1-no-soter"}
    for item in template.iterdir():
        if item.name in excluded or item.name.endswith(".sha256"):
            continue
        destination = stage_dir / item.name
        if item.is_symlink():
            raise ValueError(f"Template symlinks are unsupported: {item}")
        if item.is_dir():
            shutil.copytree(item, destination)
        else:
            shutil.copyfile(item, destination)


def copy_project_documents(stage_dir: Path) -> None:
    documents = (
        (REPO_ROOT / "README.md", stage_dir / "README.md"),
        (REPO_ROOT / "LICENSE.md", stage_dir / "LICENSE.md"),
        (REPO_ROOT / "LICENSE-2", stage_dir / "LICENSE-2"),
        (REPO_ROOT / "webui/LICENSE.upstream", stage_dir / "THIRD_PARTY_LICENSES/Tricky-Addon-Update-Target-List.txt"),
        (REPO_ROOT / "third_party/zygisk-api/LICENSE", stage_dir / "THIRD_PARTY_LICENSES/zygisk-api-0BSD.txt"),
    )
    for source, destination in documents:
        if not source.is_file():
            raise FileNotFoundError(f"Required document/license file not found: {source}")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)


def write_text_lf(path: Path, content: str) -> None:
    with path.open("w", encoding="utf-8", newline="\n") as stream:
        stream.write(content)


def normalize_module_text_files(stage_dir: Path) -> None:
    for relative_path in MODULE_TEXT_FILES:
        path = stage_dir / relative_path
        if path.is_file():
            write_text_lf(path, path.read_text(encoding="utf-8").replace("\r\n", "\n").replace("\r", "\n"))


def configure_template_for_abi(stage_dir: Path, abi: str) -> None:
    customize = stage_dir / "customize.sh"
    text = customize.read_text(encoding="utf-8")
    write_text_lf(customize, text.replace('SUPPORTED_ABIS="arm64 x64"',
                                       f'SUPPORTED_ABIS="{ABI_TO_MODULE_ARCHES[abi]}"'))


def configure_template_for_variant(stage_dir: Path, variant: str) -> None:
    if variant not in r1.VARIANTS:
        raise ValueError(f"Unsupported variant: {variant}")
    if variant == "full":
        return
    policy = stage_dir / "sepolicy.rule"
    text = policy.read_text(encoding="utf-8")
    soter_block = (
        "# Native SoterServer payload and its root-created inherited channels.\n"
        "allow { platform_app system_app } system_file unix_dgram_socket { getattr read recvfrom }\n"
        "allow system_file { platform_app system_app } unix_dgram_socket sendto\n"
        "allow { platform_app system_app } system_file unix_stream_socket { getattr read write getopt setopt shutdown }\n"
        "allow { platform_app system_app } system_file file { execute getattr map read receive }\n\n"
    )
    if text.count(soter_block) != 1:
        raise ValueError("Expected one explicit Native SoterServer policy block")
    text = text.replace(soter_block, "", 1)
    if variant == "lite":
        engineer_comments = {
            "# Permit only the verified EngineerMode application domain to expose argv[0]",
            "# for exact UID-1000 caller binding. Core system domains remain blocked.",
            "# The exact EngineerMode process keeps its inherited Zygisk companion channel.",
            "# EngineerMode INCLUDE_UNIQUE_ID attestation (Attestation Key ID Modify).",
            "# Ommega 9583e028: keep the standard permission gate, grant only this domain.",
        }
        text = "\n".join(line for line in text.splitlines()
                         if line not in engineer_comments
                         and not (line.startswith("allow ") and "engineermode_app" in line.split())) + "\n"
    write_text_lf(policy, text)
    if variant == "lite":
        service = stage_dir / "service.sh"
        text = service.read_text(encoding="utf-8")
        anchor = "# Legacy property adjustments"
        if text.count(anchor) != 1:
            raise ValueError("Expected one explicit legacy property adjustments boundary")
        text = text.split(anchor, 1)[0].rstrip() + "\n"
        required = ('"$R1_HELPER" monitor &',
                    'start_daemon "$MODDIR/daemon" "$STATE_DIR/keymint-daemon.pid"',
                    'start_daemon "$MODDIR/daemon-injector" "$STATE_DIR/injector-daemon.pid"')
        if any(line not in text for line in required):
            raise ValueError("Lite service must retain its helper and core daemon startup")
        write_text_lf(service, text)


def build_webui(stage_dir: Path, variant: str) -> None:
    env = os.environ.copy()
    env.update(VITE_R1_VARIANT=variant, VITE_R1_NAME=r1.module_name(variant),
               VITE_R1_OUT_DIR=os.fspath((stage_dir / "webroot").resolve()))
    run(["pnpm", "--dir", "webui", "build"], env=env)
    r1.load_webroot(stage_dir / "webroot")


def builder_digests() -> dict:
    paths = ("build.py", "scripts/package_r1.py", "scripts/build_engineer_bridge.py")
    return {path: r1.file_digest(REPO_ROOT / path) for path in paths}


def build_package_for_abi(*, abi: str, release: bool, platform: int, variant: str,
                          output: Path, source: dict, upstream_version: str) -> Path:
    target = ABI_TO_TARGET[abi]
    # Kept for compatibility; .cargo/config.toml owns the Android API/linker.
    _ = platform
    target_root = cargo_target_root()
    target_root.mkdir(parents=True, exist_ok=True)
    r1.check_disk(output)
    with tempfile.TemporaryDirectory(prefix=f"r1-{variant}-{abi}-", dir=target_root) as temporary:
        stage_dir = Path(temporary)
        copy_template_files(stage_dir)
        copy_project_documents(stage_dir)
        normalize_module_text_files(stage_dir)
        configure_template_for_abi(stage_dir, abi)
        configure_template_for_variant(stage_dir, variant)
        for spec in BINARY_SPECS:
            binary = build_binary(abi=abi, target=target, release=release,
                                  package=spec["package"], bin_name=spec["bin"], variant=variant)
            # Cargo's root artifact path is shared by feature sets. Copy before
            # another variant or binary build has any opportunity to replace it.
            copy_binary(binary, spec["output_name"], abi, stage_dir)
        build_helper(target=target, release=release, stage_dir=stage_dir)
        if variant != "lite":
            build_engineer_bridge(target)
            payload = build_cdylib(abi=abi, target=target, release=release,
                                   package=PIF_SPOOF_PACKAGE, library_name=PIF_SPOOF_LIBRARY)
            copy_zygisk_payload(payload, abi, stage_dir)
        build_webui(stage_dir, variant)
        normalize_module_text_files(stage_dir)
        name, entries, manifest = r1.finalize_variant(
            stage_dir, variant, source=source, build_type="release" if release else "debug",
            upstream_version=upstream_version, builder_sha256=builder_digests(),
        )
        destination = output / name
        r1.check_disk(output)
        reused = r1.write_zip_or_reuse(destination, entries)
        if reused:
            print(f"Reused identical ZIP: {destination}", flush=True)
        write_zip_sidecar(destination)
        loader, worker = build_skroot_loader(target=target, release=release, variant=variant,
                                             stage_dir=stage_dir)
        sk_name, sk_entries, _ = r1.finalize_skroot_variant(
            entries, loader=r1.read_input(loader), worker=r1.read_input(worker),
            variant=variant, module_manifest=manifest,
        )
        sk_output = output / "skrootpro"
        sk_output.mkdir(exist_ok=True)
        sk_destination = sk_output / sk_name
        r1.check_disk(sk_output)
        reused = r1.write_zip_or_reuse(sk_destination, sk_entries)
        if reused:
            print(f"Reused identical ZIP: {sk_destination}", flush=True)
        write_zip_sidecar(sk_destination)
        print(f"Output: {sk_destination}", flush=True)
        return destination


def write_zip_sidecar(destination: Path) -> None:
    sidecar = destination.with_suffix(destination.suffix + ".sha256")
    expected = r1.file_digest(destination) + "  " + destination.name + "\n"
    try:
        with sidecar.open("x", encoding="ascii") as stream:
            stream.write(expected)
    except FileExistsError:
        if sidecar.is_symlink() or r1.read_input(sidecar) != expected.encode("ascii"):
            raise FileExistsError(f"Existing checksum differs; kept unchanged: {sidecar}") from None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--release", action="store_true", help="Build in release mode")
    mode.add_argument("--debug", action="store_true", help="Build in debug mode (default)")
    parser.add_argument("--abi", dest="abis", action="append", choices=sorted(ABI_TO_TARGET),
                        help="Android ABI (default: arm64-v8a)")
    parser.add_argument("--variant", choices=("all", *r1.VARIANTS), default="full",
                        help="Source feature set; all builds three isolated packages (default: full)")
    parser.add_argument("--output", type=Path, default=TARGET_ROOT,
                        help="Directory for ZIPs and their SHA-256 sidecars (existing files are kept)")
    parser.add_argument("--platform", type=int, default=DEFAULT_PLATFORM,
                        help="Compatibility option; .cargo/config.toml owns the Android API/linker")
    args = parser.parse_args(argv)
    output = args.output.expanduser().resolve()
    output.mkdir(parents=True, exist_ok=True)
    variants = r1.VARIANTS if args.variant == "all" else (args.variant,)
    selected_abis = list(dict.fromkeys(args.abis or ["arm64-v8a"]))
    try:
        r1.check_disk(output, entry=True)
        source = get_source_info()
        upstream_version = get_version_from_cargo_toml()
        print(f"Building R1 {r1.BRAND}, source={source['head']}, dirty={source['dirty']}", flush=True)
        packages = []
        for variant in variants:
            for abi in selected_abis:
                packages.append(build_package_for_abi(
                    abi=abi, release=args.release, platform=args.platform, variant=variant,
                    output=output, source=source, upstream_version=upstream_version,
                ))
        for package in packages:
            print(f"Output: {package}", flush=True)
    except (OSError, ValueError, RuntimeError) as error:
        print(f"R1 build stopped: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
