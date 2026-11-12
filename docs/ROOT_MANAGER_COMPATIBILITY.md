# Root-manager integration

The standard module ZIP uses the documented KernelSU, APatch and Magisk
installation and boot-script contracts. Installation runs while Android is
running. The Android production gate targets ARM64, Android 12-17; a shell
installer fixture is not an on-device compatibility result.

| Manager | Installer identity | Boot lifecycle used | Property interface |
| --- | --- | --- | --- |
| KernelSU | `KSU=true` | `post-fs-data.sh`, `service.sh`, `uninstall.sh` | `/data/adb/ksu/bin/resetprop`, or a verified `ksud resetprop` capability |
| APatch | `APATCH=true` | `post-fs-data.sh`, `service.sh`, `uninstall.sh` | `/data/adb/ap/bin/resetprop`, or a verified `apd resetprop` capability |
| Magisk | Manager-owned `install_module` and Magisk tools | `post-fs-data.sh`, `service.sh`, `uninstall.sh` | `/data/adb/magisk/resetprop` or a verified PATH executable |
| SKRootPro | Native shared-library entry and metadata | Native loader calls the shared preparation/start path | An actual resetprop executable must be supplied or otherwise available |

The installer checks APatch and KernelSU identity flags before Magisk because
both managers export compatibility `MAGISK_VER`/`MAGISK_VER_CODE` values.
Framework version numbers do not select features or exclude a manager. Merely
finding an old Magisk executable also does not establish that two root engines
are active. Runtime property tools must exist, be executable, and return the
expected `ro.build.version.sdk` value through their `-n` interface.

## Package and startup

`payload.manifest` inventories the actual files shipped by each variant.
`customize.sh` extracts the manifest through the existing SHA-256 verifier,
rejects absolute paths, parent-directory traversal and installer-reserved
entries, then individually extracts and verifies each payload item. The
manifest excludes its own entry, SHA-256 sidecars, and installer internals.
The package supplies a sidecar for the manifest itself.

The shared required payload includes module metadata, boot/uninstall scripts,
SELinux rules, `r1-build.json`, `arm64-v8a/r1-helper`, and the selected ABI's
KeyMint and injector executables. PIF Zygisk libraries, engineer libraries and
DEX files are extracted when the variant's inventory contains them. A lite
inventory has no implied dependency on those optional files. Installer
permissions are applied after extraction; executable files are explicitly
marked executable.

`post-fs-data.sh` prepares OMK assets and replays a manually synchronized
system/vendor date only when a version-1 defaults snapshot and four equal exact
trust dates are present. The date behavior is described in
[Runtime security-patch dates](SECURITY_PATCH_DATES.md). The bounded shell replay
uses a verified `resetprop -n` interface without property-service notifications.
It makes no network requests, starts no Binder service, and changes no boot
property or image. All-auto startup does not synchronize dates.

`service.sh` starts the existing KeyMint/injector supervisors and the packaged
R1 helper monitor. When the documented `KSU_LATE_LOAD=1` mode skips
`post-fs-data`, it runs the same preparation before starting daemons. This
fallback also prepares a missing OMK data directory. Late-load cannot change
values that an already-running Java process cached before module startup.
The shell does not emulate optional native features by editing configuration.

Module disable and update preserve OMK data. The manager's uninstall hook
removes only the two fixed OMK roots, `/data/adb/omk` and
`/data/misc/keystore/omk`; it is shared across KernelSU, APatch and Magisk.
An uninstall performed after boot does not retroactively refresh other
processes' cached values.

KernelSU and APatch document that scripts, property changes and SELinux rules
do not require system file mounting. This module's script-driven core does not
add a metamodule requirement. KSU/APatch do not ship a built-in Zygisk engine;
features that load the optional Zygisk payload require a separate compatible
Zygisk implementation. That requirement is separate from the core daemon and
security-patch transaction.

## SKRootPro native contract

The official SKRootPro SDK documents ZIP modules containing an Android JNI
shared library and optional resource files. The manager loads the library in
user space, before `zygote64` for an ordinary boot. It passes the module's
private resource directory to the entry point. Its documented user-facing
entry is:

```c
int skroot_module_main(const char *root_key, const char *module_private_dir);
```

The public SDK expands this into the exported C symbol
`__skroot_module_main_18fdf393885363712349ef3de5411bd5` and tagged module
metadata. The module ID is a stable 32-character alphanumeric string. The
SDK also defines separate tagged install/uninstall callback exports. Resource
files are released into the manager-supplied directory with mode `0777`;
a native loader must set restrictive permissions before executing its own
resources. The entry should return quickly and fork background work that waits
for the required Android environment.

This contract does not define `KSU=true`, `APATCH=true`, a `SKROOT` shell
variable, `customize.sh`, Magisk's `install_module`, `sepolicy.rule` processing,
a resetprop path, or a KSU WebUI bridge. A native SKRootPro package must supply
its own verified loader and required capabilities. A root-manager name alone
does not prove property, SELinux, Binder, or WebUI support. On-device native
loading, resetprop availability, policy permissions and lifecycle behavior
remain separate acceptance checks.

## Official references and verification scope

The following public sources were accessed for the integration contract:

- [KernelSU module guide](https://kernelsu.org/guide/module.html): installer
  flags, BusyBox, boot stages, late-load and metamodule scope.
- [KernelSU CLI source](https://github.com/tiann/KernelSU/blob/main/userspace/ksud/src/cli.rs):
  the actual `resetprop` subcommand.
- [APatch module guide](https://apatch.dev/apm-guide.html): installer identity,
  BusyBox paths, module scripts and SELinux handling.
- [APatch CLI source](https://github.com/bmax121/APatch/blob/main/apd/src/cli.rs):
  the actual `resetprop` subcommand.
- [Magisk developer guide](https://topjohnwu.github.io/Magisk/guides.html):
  manager-owned installers and boot/uninstall hooks. Magisk may replace the
  ZIP's update-binary with its own current installer.
- [SKRootPro official SDK](https://github.com/abcz316/SKRoot-linuxKernelRoot/tree/master/Pro(%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD)/src/testModule),
  including its module-development DOCX, Hello World example,
  `module_descriptor.h` and `private_desc_parser.h`: native resource layout,
  lifecycle, metadata and exact exported ABI.

Temporary ZIP fixtures exercise KernelSU/APatch/Magisk installer identities
with standard/lite payload inventories and invalid manifest paths. Shell syntax
checks cover all edited module scripts. Rust tests cover the local transaction
and restoration behavior. These checks establish source/packaging behavior;
real manager boot ordering and Android 12-17 on kernels 4.14-6.18/newer LTS
require platform test evidence before being reported as validated.
