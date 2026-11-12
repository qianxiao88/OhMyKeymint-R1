# R1 Runtime

Independent Rust helper for `aarch64-linux-android` (Android 12-17).
Binary: `r1-helper`. This directory is its own Cargo workspace. Packaging
supplies the Android linker, `r1-build.json`, and monitor startup.

## Runtime

Packaged root commands:

```sh
/data/adb/modules/oh_my_keymint/arm64-v8a/r1-helper status
/data/adb/modules/oh_my_keymint/arm64-v8a/r1-helper cloud-update
/data/adb/modules/oh_my_keymint/arm64-v8a/r1-helper monitor
```

The module root is fixed at `/data/adb/modules/oh_my_keymint`. The private
state directory is `/data/misc/keystore/omk/data/r1/` (0700). Unit tests inject
private temporary module/state roots; the production CLI has no overrides.
The packager supplies:

```json
{"version":"1.3.8-302-full","hash":"abc1234","variant":"full"}
```

Versions accept 1-80 ASCII alphanumeric or `.-_+` characters; hashes accept
7-64 hexadecimal characters. Descriptions use the final seven hash characters.
Variants are `full`, `no-soter`, or `lite`. Packaging owns `author`.

- `status` runs the module or hot-update KeyMint's lightweight
  `--webui-get-runtime-health` command. It does not initialize Binder or query
  hardware/RKP. KeyMint must match the deployed image; keystore2 must be the
  actual system executable; injector readiness requires a verified executable
  payload mapping in that keystore2 process. A supervisor alone is configured,
  not running, and unreadable mappings are unknown. Process/mapping evidence
  does not establish the injected RPC session's liveness.
  A failed health sample is confirmed once after 300 ms, with a five-second
  deadline per command. JSON `ok` and `reason` describe local health.
  `phase` is `running`, `checking`, `degraded` or `failed`; `cloud_phase` is
  `connected`, `pending` or `offline`. `cloud_connected`, `cloud_reason` and
  `cloud_checked_at` describe verified cloud observations from the current boot.
  Missing, old-boot, corrupt, expired or future observations remain pending.
  A successful verified stats response or validated Keybox update proves a
  connection; a Keybox error remains separate in `keybox_update`/`keybox_reason`.
  `keybox_checked_at` and `next_cloud_check_at` retain update/schedule details
  without key material. Original `ok`, `reason`, `cloud_connected` consumers
  remain compatible. This command makes no network request or persistent write.
- `monitor` holds `monitor.lock` for its lifetime, waits for Android boot
  completion, and diagnoses locally every 60 seconds. Local failures have a
  120-second startup grace. After a healthy observation, a new failure gets a
  30-second checking window; monitoring retries after five seconds while checking
  and reports persistent failures rather than retaining an old success. Failed
  boot-property probes retry instead of terminating the monitor. After the local
  services become healthy it checks
  stats and automatically downloads/installs a changed cloud Keybox, then
  repeats about every six hours. A restarted monitor checks immediately after
  readiness when the previous check succeeded; an outstanding failure backoff
  is preserved across restarts. Local failure delays cloud work until services
  recover. Cloud errors do not change local health or terminate monitoring.
- Failed cloud checks retry after 15 minutes, 30 minutes, one hour, two hours,
  four hours, then at most every six hours. A fully successful check resets
  the backoff. The schedule is stored atomically with the cloud observation.
  If writing that schedule fails, the running monitor still backs off for at
  least 15 minutes rather than contacting the cloud every local polling cycle.
- `cloud-update` performs an immediate serialized check, bypassing the schedule.
  It prints `{"ok":true,"unchanged":false}` for a confirmed import, or
  `{"ok":true,"unchanged":true}` when no import is needed. Network, integrity,
  native validation, acknowledgement, and state-write failures return errors.
  Description-refresh failures are reported separately after the update result.

## Download, Cache And Import

All update operations use the same nonblocking `update.lock`: the monitor
skips a busy updater, while manual callers receive a busy result. The lock
covers network requests, import and atomic cache/schedule writes. Files stay
in place so process death releases kernel `flock` without creating a second
lock inode. `description.lock` separately serializes property updates.

On HTTP 200 the helper reads at most 64 KiB, requires an exactly 64-hex
`X-Keybox-Sha256` header, verifies SHA-256 and compares the local file byte for
byte. Identical content skips staging, native import and watcher reload.
Only the small observation/cache metadata is saved once per completed check;
no Keybox is rewritten, no restart is requested, and identical descriptions
also skip their write.

Changed content is staged in a create-new 0600 temporary file in the private
directory, synced and reverified. The existing native
`--webui-install-keybox <base64 chunks>` performs XML/private-key/certificate
validation and atomic metadata-preserving replacement. Base64 chunks are
48 KiB, matching WebUI. The helper requires `ok` and then verifies the installed
file's digest before advancing its successful cache. The existing watcher
reloads KeyMint; the helper sends no additional signal or restart command.
It never writes the live Keybox itself.

If provided, HTTP `ETag` and `Last-Modified` are stored only after confirmed
installation or byte equality. A later request sends `If-None-Match` and/or
`If-Modified-Since` only while the current local file matches that successful
SHA-256. Missing or changed local files require an unconditional full download.
HTTP 304 is accepted only for such a conditional request, with a second local
digest check. An optional SHA-256 header on 304 must also match. No 304 body is
read, no import is attempted, and returned validators update the same cache.
A 200 without validators clears old validators. Header values are bounded and
sanitized before storage/use. Failed imports retain the previous successful
cache and timestamp, so a failed version never becomes a successful 304 hit.

Native stdout and network bodies are bounded; native stderr is suppressed.
Errors never include downloaded XML, Base64, private keys, HTTP response bodies
or hashes. The native interface carries Base64 in argv for the short import
lifetime. A file/stdin interface would require a change outside this crate.
An import timeout or unexpected acknowledgement may follow a native replacement;
the next check compares actual installed bytes before deciding to import again.
Temporary files are removed on every normal/error/unwind path. SIGKILL or power
loss can leave private staging files; they are never imported automatically.

## Cloud Protocol And Bounds

```text
GET https://omk.5ijxa.com/api/omk/index.php?action=stats
GET https://omk.5ijxa.com/api/omk/index.php?action=keybox
```

Requests use certificate-verified HTTPS, rustls and Mozilla WebPKI roots.
`ureq ~3.1` has default features disabled and `rustls` explicitly enabled.
Redirects, environment proxies, compression and cookies are disabled. Stats
accepts only 200; Keybox accepts 200 or a valid conditional 304. Header limit
is 8 KiB. Timeouts: DNS 5 seconds, connect/TLS 10 seconds, response headers
10 seconds, response body 15 seconds, complete request 30 seconds.

Stats body limit is 8 KiB, including responses without Content-Length. It must
be an object with nonnegative integer `devices`, `peak`, `keybox_distinct`,
`version_code`, product exactly `OhMyKeymint-R1`, and nonempty string `version`
and `updated_at`. Zero devices is valid. Stats metadata is not a Keybox version;
the Keybox endpoint's conditional response or verified bytes determine changes.
A stats failure does not prevent a valid Keybox import. A successful verified
stats response or validated Keybox response establishes connectivity. Keybox
failure remains separate from connectivity and retains the update retry backoff;
a successful Keybox response also permits normal scheduling when stats is down.

No device/module identifier is generated or uploaded. There is no POST report,
heartbeat, private-key upload, or Keybox hash upload. Persisted cloud state
contains observations, a local boot identifier used only for cache freshness,
retry schedule, successful digest and HTTP cache validators. The boot identifier
is never transmitted. Stats success does not contribute to the server's device count.

Native commands have a five-second deadline per health sample, a 20-second
import deadline, or three seconds for getprop, with nonblocking bounded stdout
and bounded process-group cleanup.
Descriptions flatten control characters, CR/LF and Unicode line separators.
Only `description=` changes; other properties and permission bits are preserved.
Replacement is synced, renamed atomically, then its parent is synced. Symlinked
`module.prop` is rejected. Ownership/SELinux labels need packaged-device testing.
Description success requires both healthy local KeyMint/keystore2/injector
services and a fresh validated cloud connection. It uses the packaged version
and final seven content-hash characters. Core JSON `ok` remains independent
from the cloud; the combined description reports a cloud connection failure
with its specific reason. Keybox update details remain in the JSON status.
Startup grace and pending/expired/old-boot cloud evidence retain a neutral
checking message until a result is available; confirmed local or cloud failures
use the requested failure text with specific reasons. Health is resampled after
network work before updating the description.

```text
✅ OMK-R1-（1.3.8-302-full）-（abc1234）已成功运行。已成功连接云端。等着被我格你机机吧😄 【本模块由【浅笑呐】二改自用，属免费模块，倒卖可耻。】
❌ OMK-R1 运行失败。（injector is stopped）你卸载吧。
```

## Validation

Use a modern Rust toolchain and the repository's configured Android NDK linker.
This crate uses no nightly language features. On the prepared environment:

```sh
export PATH=/root/.cargo/bin:$PATH
export CARGO_TARGET_DIR=/root/omk-build/r1-runtime-target
cargo +nightly fmt --manifest-path tools/r1-runtime/Cargo.toml --all -- --check
cargo +nightly clippy --manifest-path tools/r1-runtime/Cargo.toml \
  --target aarch64-linux-android --workspace --all-targets -- -D warnings
cargo +nightly test --manifest-path tools/r1-runtime/Cargo.toml \
  --target aarch64-linux-android --workspace --no-run
```

Run the generated Android test executables in an Android test environment
using a non-root test account and private temporary directory, or set an
explicit Cargo runner. Tests use mock bytes (not real XML/certificates), a
loopback HTTP fixture server and injected temporary roots; they do not contact
the cloud or invoke the product CLI against `/data`.

Tests cover local/cloud health separation, descriptions, boot grace, protocol
and response limits, hash validation, cache validators and 304, unchanged
content, native validation/acknowledgement failures, installed-byte confirmation,
backoff/restarts, atomic state-write failures, concurrent updates, property
preservation, private staging/cleanup, and command time/output limits. Host
execution supplements algorithm checks but is not Android runtime acceptance.
Real TLS outage, Binder diagnostics, native certificate rejection, watcher
reload, SELinux and service startup need device integration verification.
