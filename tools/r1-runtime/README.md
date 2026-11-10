# R1 Runtime

Independent Rust stable helper for `aarch64-linux-android` (Android 12-17).
Binary: `r1-helper`. This directory is its own Cargo workspace. The main agent
supplies Rust stable, the Android NDK linker and packaging; this implementation
has not been built, installed, or executed on an Android device.

## Runtime

Run as root using the packaged binary:

```sh
/data/adb/modules/oh_my_keymint/arm64-v8a/r1-helper status
/data/adb/modules/oh_my_keymint/arm64-v8a/r1-helper cloud-update
/data/adb/modules/oh_my_keymint/arm64-v8a/r1-helper monitor
```

The module root is fixed at `/data/adb/modules/oh_my_keymint`. The private data
directory is `/data/misc/keystore/omk/data/r1/` (0700). Unit tests inject both
roots through `Paths`; command-line root overrides are deliberately absent.
The packager supplies `r1-build.json` in the module root:

```json
{"version":"1.3.8-301-full","hash":"abc1234","variant":"full"}
```

Versions accept 1-80 ASCII alphanumeric or `.-_+` characters. Hashes accept
7-64 hexadecimal characters; descriptions use the final seven. Variants are
`full` or `lite`. `author` remains the packager's responsibility.

- `status` calls the current module's `arm64-v8a/keymint
  --webui-get-diagnostics`, requiring `keymint`, `keystore2`, and `injector`
  each to have `status: "running"` and a positive integer PID. It emits
  `{ "ok": boolean, "reason": string, "cloud_connected": boolean }`.
  Exit status is nonzero when unhealthy. Cloud connectivity comes from the
  latest actual stats observation in private `cloud.json`, valid for less
  than six hours. A missing, corrupt, expired, or future-dated observation
  fails closed. `status` itself performs no network request or persistent write.
- `monitor` holds `monitor.lock` with nonblocking `flock` for its entire life.
  Lock files stay in place; kernel locks release on process death. It waits
  for `/system/bin/getprop sys.boot_completed` to return `1`, then diagnoses
  locally every 60 seconds. Local failures receive a separate waiting
  description during the first 120 seconds after boot readiness. Cloud stats
  are fetched immediately after boot readiness and every six hours thereafter,
  including retries following failed checks. Cloud failures after local
  readiness produce a failure description. Packaging must arrange startup;
  the helper creates no service or automatic startup configuration.
- `cloud-update` serializes updates with `update.lock`, stages the response in
  a create-new 0600 temporary file, requires `X-Keybox-Sha256`, hashes the
  staged bytes, and delegates certificate/private-key validation and atomic
  installation to the existing native CLI. It then refreshes actual stats,
  local diagnostics and the module description. Successful import emits
  exactly `{"ok":true}`; subsequent status-refresh errors are warnings on
  stderr and do not change that successful import result.

## Native CLI Contract

The checked-in `src/main.rs` and `webui/src/cli.ts` expose
`--webui-install-keybox <base64 chunks>` for import, with 48 KiB chunks and an
`ok` acknowledgement. The helper uses this exact interface; a literal
`keybox import <file>` subcommand would enter the daemon path in this revision.
The additional `base64` dependency uses the same standard encoding as WebUI.
The native parser validates before its metadata-preserving atomic keybox
replacement. The existing `keybox::initialize` file watcher reloads the keybox;
the helper sends no extra reload signal and changes no authentication or
injection logic. Existing keybox bytes remain untouched until native import.
An import timeout or unexpected acknowledgement has an uncertain outcome if
the native atomic replacement already happened; verify installed state before
retrying. The helper never writes or restores the live keybox itself.

Native stdout is bounded and native stderr is suppressed. Errors never include
downloaded XML, Base64, private keys, or native error bodies. The current native
interface carries Base64 in argv, visible to privileged process inspection for
the short import lifetime. A file/stdin native import interface would require
a separate change outside this directory. Temporary files are removed through
RAII on normal success and every returned-error/unwind path. SIGKILL, power
loss and process abort can leave a 0600 staging file in the private directory;
the monitor never imports abandoned staging files.

## Cloud Protocol

Both requests use certificate-verified HTTPS with rustls and Mozilla WebPKI
roots. `ureq ~3.1` disables default features and explicitly enables `rustls`
(in 3.1 this enables the internal TLS support and WebPKI roots; there is no
public feature named `tls`). Redirects, environment proxies, compression and
cookies are disabled. Only HTTP 200 is accepted. Header limit: 8 KiB.
Timeouts: DNS 5 seconds, connect/TLS 10 seconds, response headers 10 seconds,
response body 15 seconds, whole request including DNS/body 30 seconds.

```text
GET https://omk.5ijxa.com/api/omk/index.php?action=stats
GET https://omk.5ijxa.com/api/omk/index.php?action=keybox
```

Stats body limit is 8 KiB, including responses without Content-Length. It must
be a JSON object with nonnegative integer `devices`, `peak`, `keybox_distinct`,
`version_code`, product exactly `OhMyKeymint-R1`, and nonempty string `version`
and `updated_at`. Zero devices is valid. HTML, arrays, stringified counts,
booleans, fractional counts and partial responses fail validation. This schema
matches the supplied cloud `public/index.php`. Keybox body limit is 64 KiB;
the required SHA-256 header is exactly 64 hexadecimal characters. XML validity
is decided by the original native importer. Hash verification provides
transport consistency; HTTPS authenticates the server.

No persistent module/device identifier is generated or uploaded. There is no
POST `report`, heartbeat, keybox hash upload, or private-key upload. The cloud
report protocol (`module_id` matching `^omk-[a-f0-9]{16,64}$`, optional
`keybox_sha256`, boolean `ok` plus `interval` reply) is intentionally unused.
Stats success proves stats endpoint connectivity and protocol validity; this
client will not contribute to the server's reported online-device count.
Normal HTTPS network metadata (including source IP) remains visible to the
server. Cloud state is only the verified outcome, timestamp and safe reason.

## Description And Bounds

Success requires both healthy local diagnostics and a fresh successful stats
response. Success/error copy follows the requested R1 text. Boot, service grace
and active checks have separate waiting copy. Reasons are length-limited and
flatten CR/LF, control characters and Unicode line/paragraph separators.
Only `description=` is replaced; all other module properties, including
`author`, are preserved. A short shared file lock serializes read-modify-write.
Replacement uses a same-directory temporary file, preserves permission bits,
syncs it, renames atomically and syncs the parent directory. Symlinked
`module.prop` is rejected. Mode preservation covers file permissions; SELinux
labels and ownership follow creation in the module directory and need device
verification in the packaged Magisk/KernelSU environment.

Every native invocation has a deadline: 20 seconds for diagnostics/import,
3 seconds for getprop. Stdout uses nonblocking bounded reads. Deadline/error
cleanup kills the child's dedicated process group and polls reaping for at
most one further second, avoiding an unbounded `wait`. Monitor polling and
sleeping are intentional long-lived behavior. Description lock acquisition
is limited to three seconds; duplicate monitors/updates fail immediately.

## Main-Agent Verification

Commands below are for the agent preparing the toolchain; they were not run
during implementation. Run from this directory with the Android NDK linker
already configured. Use a stable toolchain; no parent workspace configuration
or nightly feature is required by this crate.

```sh
cargo +stable fmt --all
cargo +stable fmt --all -- --check
cargo +stable clippy --target aarch64-linux-android --all-targets -- -D warnings
cargo +stable test --target aarch64-linux-android --no-run
cargo +stable build --release --target aarch64-linux-android --bin r1-helper
```

Deploy the generated `target/aarch64-linux-android/debug/deps/r1_runtime-*`
test executable to an authorized arm64 Android device and execute it there.
An explicitly configured Android Cargo runner may also use:

```sh
cargo +stable test --target aarch64-linux-android --lib
```

Tests cover descriptions, startup grace, single-line reasons, diagnostics,
cloud schema/types, body limits, hashes/build metadata, property preservation
and mode, private staging/cleanup, exclusive locks, stale cloud observations,
and subprocess timeout/output limits. Real TLS failure/timeout, Binder
diagnostics, native certificate rejection/old-keybox preservation, watcher
reload, SELinux permissions and monitor startup require Android integration
verification. No Android device validation is claimed.

The ureq 3.1.4 documentation was checked for `Agent::config_builder`, timeout
setters, `TlsConfig`, `RootCerts::WebPki`, and `Body::as_reader`. The main agent
must still compile-check the resolved 3.1 patch release and its transitive
rustls/ring Android toolchain requirements. `Cargo.lock` is to be generated and
reviewed by that build. Direct versions: ureq 3.1, serde 1, serde_json 1,
sha2 0.10, base64 0.22, libc 0.2. `libc` supplies Android-compatible flock and
process-group termination without requiring recent stable file-lock APIs.
