# SKRootPro native module contract

The SKRootPro build uses an independent Rust workspace at
[tools/skroot-loader](../tools/skroot-loader/README.md). Its `cdylib` implements
SKRootPro's native module entry and callbacks. A separate Rust worker performs
installation, policy checks and supervision. Android arm64 is the production target;
the loader calls no SDK kernel function and does not implement a Zygisk provider.

## Upstream ABI evidence

The following master-branch official resources were actually read:

- [private_desc_parser.h](https://raw.githubusercontent.com/abcz316/SKRoot-linuxKernelRoot/master/Pro%28%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD%29/src/testModule/kernel_module_kit/include/third_party/____DO_NOT_EDIT____/private_desc_parser.h), in full.
- [module_descriptor.h](https://raw.githubusercontent.com/abcz316/SKRoot-linuxKernelRoot/master/Pro%28%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD%29/src/testModule/kernel_module_kit/include/module_descriptor.h).
- [hello-world example](https://raw.githubusercontent.com/abcz316/SKRoot-linuxKernelRoot/master/Pro%28%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD%29/src/testModule/module_hello_world/module_hello_world.cpp), [background example](https://raw.githubusercontent.com/abcz316/SKRoot-linuxKernelRoot/master/Pro%28%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD%29/src/testModule/module_background_service/module_background_service.cpp), and [install/uninstall example](https://raw.githubusercontent.com/abcz316/SKRoot-linuxKernelRoot/master/Pro%28%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD%29/src/testModule/module_install_uninstall_cb_demo/module_install_uninstall_cb_demo.cpp).
- [SKRoot module development guide DOCX](https://raw.githubusercontent.com/abcz316/SKRoot-linuxKernelRoot/master/Pro%28%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD%29/src/testModule/SKRoot%E6%A8%A1%E5%BC%80%E5%8F%91%E6%8C%87%E5%8D%97.docx), decoded from an HTTP in-memory ZIP, without retaining a probe copy.
- [module_base_install_callback.h](https://raw.githubusercontent.com/abcz316/SKRoot-linuxKernelRoot/master/Pro%28%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD%29/src/testModule/kernel_module_kit/include/module_base_install_callback.h) and [skroot_root.h](https://raw.githubusercontent.com/abcz316/SKRoot-linuxKernelRoot/master/Pro%28%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD%29/src/testModule/kernel_module_kit/include/skroot_env/skroot_root.h).

Exact exports:

```c
int __skroot_module_main_18fdf393885363712349ef3de5411bd5(
    const char *root_key, const char *module_private_dir);
void __skroot_module_on_install_k6JkibrjUwzXFpshvFuIwdUNRP4fYtLA(
    const char *root_key, const char *module_private_dir,
    void (*callback)(const char *reason));
void __skroot_module_on_uninstall_epmoso85hw4w1rzjagiuzdmnyqamk7ep(
    const char *root_key, const char *module_private_dir);
```

The install callback calls `callback("")` only after validation and installation
complete; a nonempty reason stops upstream installation. The main entry runs before
Zygote64, forks rapidly, and returns zero when dispatch is accepted. Zero is not a
claim that daemons or Binder are healthy. The child creates a session and execs the
worker; file copying, policy queries, delays and shell startup happen in the worker.
Only async-signal-safe libc operations run between fork and exec. The worker waits
five seconds, matching the descriptor's recommendation for background work.

`root_key` is ignored: no copy, argv, environment variable, log or persistent state
contains it. The private resource path is transferred over an anonymous socket to
worker stdin, rather than argv or environment. No part of that path is persisted
or logged, because SKRoot private ancestors may include a key-derived prefix.
Worker environment is explicit and contains no invented SKROOT variables.

All five required descriptor strings use the exact upstream begin/end markers,
NUL-terminated exported byte arrays, retained through LTO/strip. Install/uninstall
flags are retained too. The ID is 32 ASCII letters/digits. The official kit archive
was inspected in memory using `llvm-dis`: `module_min_sdk.o` retains the minimum
SDK `4.3.7` marker. This implementation uses the same marker, not an Android API
level in that field.

The same inspection of `find_module_elf_in_zip` confirms that the framework scans
root-level files (also allowing `./`) and attempts metadata parsing; it is not
restricted to a fixed basename or `.so` suffix. Therefore the sole root-level
metadata ELF is `libomk_skroot_loader.so`; the worker is in a subdirectory.

## Build and ZIP contract

Do not add this workspace to the main Cargo workspace or alter root build files
for it. Use a separate executable target directory, since Android shared storage
may be mounted noexec. From `tools/skroot-loader` with the Android linker configured:

```sh
export PATH=/root/.cargo/bin:$PATH
export CARGO_TARGET_DIR=/root/workspace/omk-skroot-loader-target
export OMK_SKROOT_VERSION=1.3.8-302-lite
export OMK_SKROOT_VARIANT=lite
cargo +stable fmt --all -- --check
cargo +stable clippy --target aarch64-linux-android --all-targets -- -D warnings
cargo +stable test --target aarch64-linux-android --no-run
cargo +stable build --release --target aarch64-linux-android
```

Supported build variants are `full`, `no-soter`, `lite`; the same Rust implementation
is rebuilt with identity fields. `OMK_SKROOT_VERSION` must exactly equal the manifest
version. Optional compile-time fields: `OMK_SKROOT_NAME`, `OMK_SKROOT_AUTHOR`,
`OMK_SKROOT_DESC`, `OMK_SKROOT_ID32`. All three variants should retain the default
ID `3e5017f4de9b4d86a25d7d320fbd29e2` so they update one module slot. Identity
strings are bounded printable ASCII. Runtime environment does not override them.

Produce a separate SKRootPro ZIP for each variant, preserving the requested ZIP
filename within the SKRootPro output subdirectory. Keep these entries:

```text
libomk_skroot_loader.so          # release cdylib; top-level metadata ELF
skroot/omk-skroot-worker         # release executable; ZIP mode 0755
omk-payload.json                 # generated manifest, mode 0644
payload/module.prop
payload/post-fs-data.sh
payload/service.sh
payload/daemon
payload/daemon-injector
payload/sepolicy.rule
payload/arm64-v8a/r1-helper
payload/libs/arm64-v8a/keymint
payload/libs/arm64-v8a/inject
payload/...                     # complete matching OMK module resource tree
```

The `payload` tree is byte-identical to the corresponding standard module tree,
including `module.prop` and R1 identity. Do not duplicate ARM64 keymint/inject under
`arm64-v8a`: the standard `libs/arm64-v8a` layout is required. The R1 helper stays
at `arm64-v8a/r1-helper`. Include every intended resource in the manifest, including
WebUI, notices, `r1-build.json`, variant flags, optional `bin/resetprop` and Zygisk
libraries where applicable. Do not list the manifest, cdylib or worker in `files`.
Hashes are for payload files only, avoiding recursive self-hashes. Full ZIP hashes
belong in a separate release checksum file.

```json
{
  "schema": 1,
  "id32": "3e5017f4de9b4d86a25d7d320fbd29e2",
  "version": "1.3.8-302-lite",
  "variant": "lite",
  "files": [
    {"path": "module.prop", "mode": 420, "sha256": "<64 hex characters>"}
  ]
}
```

This JSON is a schema example; the actual file must list all required resources.
Sort entries by path. Modes are decimal `420` (0644), `493` (0755) or `384` (0600).
Startup scripts, daemons, native ARM64 binaries, helper and optional resetprop must
be 0755. Paths are ASCII relative paths, maximum 512 bytes, and use only letters,
digits, slash, dash, underscore, dot and plus. No symlinks, hardlinks, devices,
empty components, dot/dot-dot traversal, lifecycle markers, reserved owner files,
duplicate entries or unlisted payload files are accepted. Each regular file is
bounded to 512 MiB; manifest is bounded to 2 MiB and 8192 entries.

The loader validates the declared build identity, complete file set and SHA-256,
then copies resources into a same-parent staging directory. It fixes directory/file
modes, syncs files and writes `.omk-skroot-owner.json` there. Initial publication
uses `renameat2(RENAME_NOREPLACE)`; update uses `RENAME_EXCHANGE`. There is no
non-atomic delete-and-copy fallback. Filesystem/kernel support for these operations
is a prerequisite. Aborted staging trees are never used as installed modules.

## Fixed paths and lifecycle

The installed module root is exactly `/data/adb/modules/oh_my_keymint`, compatible
with the R1 runtime and existing daemon/injector fallback paths. The directory is
created only when absent, or replaced only when a regular owner marker matches this
loader's schema and ID32. A directory installed by another manager, symlinked
ancestors, malformed ownership or disabled/pending-removal modules stop the operation.
The loader changes no root manager selection, binaries, settings or install markers.

Private loader state is `/data/misc/keystore/omk/data/skroot-loader` (0700). The
lifetime flock serializes duplicate boot entries; the operation flock serializes
installation/removal. An update requests worker stop and waits up to ten seconds
before publishing. Installation is synchronous and leaves services stopped until
reboot. Worker watches the fixed root's `disable`/`remove` and its private stop
request. The supervising worker owns a dedicated process group, terminates that
group on stop/failure, and checks group membership before escalating surviving
children. It does not kill processes merely by shared PID files or command names.

Startup reuses the packaged `post-fs-data.sh`, then `service.sh`, which in turn
uses the existing `daemon` and `daemon-injector`. Script children inherit the worker's
root identity and SELinux SID; there is no KSU/APatch script host. The loader puts
`MODULE_ROOT/bin` before Android system directories in child PATH, for a separately
provided real resetprop. `post-fs-data.sh` is bounded to 60 seconds; the service
script is supervised. R1 monitor's own flock prevents duplicate monitor work.

Uninstall stops this worker and atomically moves/deletes only the marked fixed
module tree. It does not execute the generic uninstall shell script, remove another
manager's files, delete shared `/data/adb/omk` state, remove keystore data, or rewrite
the device's root manager. Shared OMK configuration and keyboxes are retained.

`status.json` is atomic and 0600. Phases are `installed`, `starting`, `dispatched`,
`blocked`, `stopped`, `removed`. `dispatched` means scripts were launched, not that
Binder/injection passed. Actual healthy state must come from `r1-helper status` and
the existing native diagnostics. A policy/package error is never marked ready.

## Effective SELinux policy prerequisite

SKRoot does not provide Magisk/KSU `sepolicy.rule` loading. A copied rule file is
not proof of applied permissions. The checked public SDK headers expose no rule
loader. In-memory inspection of the official archive found that root exception
logic checks root credentials, seccomp and full capabilities. That does not prove
access for UID1017 after the native daemon drops UID, or for the payload in
keystore2. The default framework's `module_selinux.o` contains kernel enforcing/
status operations; this loader neither links it nor calls those operations.

Before installing/starting scripts, the loader requires Android enforcing and opens
`/system/lib64/libselinux.so`, resolving genuine platform APIs:
`getcon_raw`, `freecon`, `string_to_security_class`, `string_to_av_perm`, and
`security_compute_av_flags_raw`. The API declarations were checked against
[Android 17 libselinux](https://android.googlesource.com/platform/external/selinux/+/refs/heads/android17-release/libselinux/include/selinux/selinux.h).
It requires actual allowed bits, rejecting permissive-domain flags, missing APIs,
unknown contexts/classes/perms, and query failures. It checks the current daemon
SID's UID/GID capability, socket creation and executable mapping requirements, then
every allow in the packaged `sepolicy.rule`. Sets are expanded; permission `*` is
expanded from the active kernel class/perms directory. Unsupported rule syntax
stops preflight rather than being silently skipped.

This is a check of an already effective policy, not a policy installer. A device
policy or separately installed real policy provider must supply every requirement
before this package's install callback succeeds. Stock SKRootPro with stock Android
policy is not established as a working combination. In particular the keystore to
system_file IPC/mapping permissions and variant-specific EngineerMode/Soter
requirements need actual effective allows. A missing requirement produces a
concrete context/class/permission failure in private status. The loader never
sets enforcing to zero or fabricates healthy state.

Executable resources are labeled `u:object_r:system_file:s0` through the actual
Android xattr interface after installation. Label failures stop startup. Device
policy must also allow the worker to label/access its module tree and the existing
post-fs script's keystore storage operations. Passing AV queries is a prerequisite,
not proof that all Binder, file-context, ptrace/injection and boot timing behavior
works on a particular ROM.

## Other explicit dependencies and validation limits

- Full/no-soter PIF requires a genuine external Zygisk implementation/provider that
  recognizes the standard module's `zygisk` payload at the fixed module path. The
  loader neither installs one nor claims that a `.so` file alone starts Zygisk.
- SKRoot exposes no compatible resetprop interface here. Supply a real executable
  as `payload/bin/resetprop` with its notice if property mutation is required. The
  native manager probes verify a real `-n` read interface; the loader invents no
  SKRoot property command or manager path. Missing resetprop leaves property-driven
  features subject to their existing explicit errors/skips.
- Executing `post-fs-data.sh` in a five-second-delayed child does not establish
  Magisk's exact early-post-fs ordering. Replaying properties before Android
  framework caching requires device timing validation.
- SKRoot's private directory is trusted ABI input and resource provenance comes
  from the installed package, not a network-signature mechanism. It must be a
  real absolute private directory under `/data`, outside other managers' shared
  adb/misc/user directories; no fixed secret-derived directory name is assumed.
- Android 12-17/kernel 4.14-6.18 compatibility is a target, not a claim of tests
  across those systems. No live device path, root key, real module entry,
  installer callback, SELinux mutation or daemon was invoked during development.

The independent Android fmt/Clippy/all-target test-link gate passed. Six Linux
mock tests passed using isolated temporary directories: metadata, lifecycle lock,
policy parser, manifest/path validation, digest/symlink rejection, and owned atomic
install/update/disable/remove with foreign-manager preservation. These mocks do not
replace execution of Android test binaries or ROM/SKRoot/SELinux/Zygisk integration.

Source/licensing notes are in
[LICENSE-NOTES.md](../tools/skroot-loader/LICENSE-NOTES.md).
