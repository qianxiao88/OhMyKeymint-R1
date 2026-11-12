# Runtime security-patch dates

The WebUI **Sync security patch** action explicitly updates two global Android
runtime properties:

- `ro.build.version.security_patch`;
- `ro.vendor.build.security_patch`.

It also stores the same date in the four `[trust]` fields `security_patch`,
`os_patchlevel`, `vendor_patchlevel`, and `boot_patchlevel` in `config.toml`.
These trust fields control OMK attestation values; setting `boot_patchlevel`
does not write a boot property or image. Per-application patch-level profiles
retain their explicit configured overrides.

Synchronization fetches the newest published date from the official Android
Security Bulletin overview. The saved system default determines its `01`/`05`
day convention: an original system date ending in `01` keeps day `01` when the
published date ends in `05`; other original days keep the published date. The
native helper validates the selected date before changing state. Ordinary
all-auto startup leaves the properties and configuration unchanged.

## Snapshot and restoration

Before the first manual synchronization, the helper atomically saves the
original pair and build fingerprint at
`/data/misc/keystore/omk/data/security_patch_defaults.toml`, with mode `0600`
and keystore ownership. The version-1 schema contains only `version`,
`build_fingerprint`, `ro.build.version.security_patch`, and
`ro.vendor.build.security_patch`. The state must be a bounded, valid regular
TOML file.

Defaults come from runtime properties while all four trust fields are `auto`.
If an override is active, defaults come from build-property sources instead.
Repeated synchronization retains the original pair for the same build; a changed
build fingerprint refreshes defaults from build properties. The reader accepts
an existing version-2 file by extracting only its original system/vendor pair
and fingerprint into the version-1 representation. Reading does not rewrite the
file or apply its extra fields. Newly saved snapshots use only version 1.

The data-directory operation lock serializes the existing OMK writers. The
helper writes vendor before system using `resetprop -n`, verifies each result,
then verifies the pair. Android property storage does not offer a multi-key
atomic primitive, so another reader may observe intermediate values. A failed
write restores values already changed in reverse order. A failed configuration
write restores the original pair. Rollback preserves an unrelated writer's
concurrent property value and reports the conflict.

**Restore default security patch** makes no network request. It restores the
saved system/vendor pair, writes `auto` to all four trust fields, then removes
the snapshot. If there is no snapshot and an override is active, it reads the
build-property defaults. Invalid saved state produces an error before properties
or configuration are changed.

## Startup replay

Startup reapplies a manually synchronized pair only when a snapshot exists and
all four trust fields contain the same exact date. It does not create a snapshot
for an all-auto configuration or a hand-edited date without saved state.
`post-fs-data.sh` performs an early replay for version-1 snapshots before Android
framework processes cache `Build.VERSION.SECURITY_PATCH`. Normal KeyMint startup
also replays the saved override before resolving trust and refreshes the snapshot
when the build fingerprint changes. A replay error is logged and regular
startup continues.

The property writer requires an executable resetprop with the expected `-n`
interface. It checks manager-provided executables and PATH using a read-only SDK
property query. KernelSU/APatch subcommands and supported fallback executables
use the same interface. Late loading or changing properties after Zygote starts
does not refresh Java values cached by processes that are already running.

## Scope and validation

Only the two runtime properties and OMK's trust configuration are changed. No
boot, init_boot, vendor_boot, vbmeta, or system image is written. Hardware firmware
and the original system TEE's attestation output are unchanged. OMK encodes the
OS patch level as `YYYYMM` and vendor/boot levels as `YYYYMMDD`.

The Android acceptance target is `aarch64-linux-android`, Android 12-17. Validate
with:

```sh
cargo fmt --all -- --check
cargo clippy --target aarch64-linux-android --workspace --all-targets -- -D warnings
cargo test --target aarch64-linux-android --workspace --no-run
```

Execute generated Android test binaries on an explicitly selected device for a
device gate. Source tests cover manual synchronization, day conventions, repeated
sync, saved defaults, restore/removal, build changes, all-auto no-op startup,
snapshot-only replay, existing extended-file reads, write/config failures, and
concurrent changes. Compilation and shell fixtures do not establish manager boot
timing, SELinux permissions, or on-device property behavior for a ROM/kernel.
