use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, String>;

pub const MODULE_ROOT: &str = "/data/adb/modules/oh_my_keymint";
pub const DATA_ROOT: &str = "/data/misc/keystore/omk/data/r1";
const STATS_URL: &str = "https://omk.5ijxa.com/api/omk/index.php?action=stats";
const KEYBOX_URL: &str = "https://omk.5ijxa.com/api/omk/index.php?action=keybox";
const KEYBOX_PATH: &str = "/data/misc/keystore/omk/keybox.xml";
const STATS_LIMIT: usize = 8 * 1024;
const KEYBOX_LIMIT: usize = 64 * 1024;
/// Cloud polling interval when the configuration does not name one.
///
/// Ten minutes keeps a revoked keybox from lingering: the cloud re-issues on its
/// own schedule, and the client picks the replacement up on the next poll.
const DEFAULT_CLOUD_INTERVAL_MINUTES: u64 = 10;
/// Upper bound for the configurable interval (24 h), so a typo cannot park the
/// helper for a week.
const MAX_CLOUD_INTERVAL_MINUTES: u64 = 24 * 60;
/// Reference interval the freshness assertions are written against. Production
/// code reads `cloud_interval()` instead, so this only exists for the tests.
#[cfg(test)]
const CLOUD_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const CLOUD_RETRY_MIN: Duration = Duration::from_secs(15 * 60);
const LOCAL_INTERVAL: Duration = Duration::from_secs(60);
const STARTUP_GRACE: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_RECHECK_DELAY: Duration = Duration::from_millis(300);
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// Injection is a library/testing facility; the production CLI accepts no path overrides.
#[derive(Clone)]
pub struct Paths {
    pub module: PathBuf,
    pub data: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self { module: MODULE_ROOT.into(), data: DATA_ROOT.into() }
    }
}

impl Paths {
    fn keymint(&self) -> PathBuf {
        let hot_update = Path::new("/data/adb/omk/keymint");
        if hot_update.is_file() {
            return hot_update.to_path_buf();
        }
        self.module.join("libs/arm64-v8a/keymint")
    }

    fn prepare(&self) -> Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.data)
            .map_err(|_| "cannot create private R1 data directory".to_string())?;
        fs::set_permissions(&self.data, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot secure R1 data directory".to_string())
    }
}

pub fn single_line(text: &str) -> String {
    clean_line(text, 240)
}

fn clean_line(text: &str, limit: usize) -> String {
    text.chars()
        .map(|c| if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') { ' ' } else { c })
        .take(limit)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn read_limited(reader: impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "response read failed or timed out".to_string())?;
    if bytes.len() > limit {
        return Err(format!("response exceeds {limit} bytes"));
    }
    Ok(bytes)
}

fn read_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = File::open(path).map_err(|_| "required file is unavailable".to_string())?;
    read_limited(file, limit)
}

// SIGKILL the command's own process group, including any children retaining stdout.
// No shell is involved; all arguments are passed directly via exec.
struct BoundedChild(Child);

impl Drop for BoundedChild {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(self.0.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn command_output(
    program: &Path,
    args: &[String],
    timeout: Duration,
    limit: usize,
) -> Result<Vec<u8>> {
    let (mut reader, writer) =
        UnixStream::pair().map_err(|_| "cannot prepare command output".to_string())?;
    reader.set_nonblocking(true).map_err(|_| "cannot bound command output".to_string())?;
    let mut child = BoundedChild(Command::new(program)
        .args(args)
        .env("LD_LIBRARY_PATH", "/system/lib64:/apex/com.android.runtime/lib64:/apex/com.android.runtime/lib64/bionic:/vendor/lib64")
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| "cannot start native command (Android/root/module required)".to_string())?);
    let started = Instant::now();
    let mut bytes = Vec::new();
    let mut eof = false;
    loop {
        if started.elapsed() >= timeout {
            return Err(
                "native command timed out; if importing, verify installed state before retrying"
                    .into(),
            );
        }
        let mut buffer = [0u8; 4096];
        match reader.read(&mut buffer) {
            Ok(0) => eof = true,
            Ok(count) => {
                if bytes.len() + count > limit {
                    return Err("native command output exceeds limit".into());
                }
                bytes.extend_from_slice(&buffer[..count]);
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("native command output read failed".into()),
        }
        if let Some(status) = child.0.try_wait().map_err(|_| "cannot inspect native command")? {
            if !status.success() {
                return Err(format!("native command failed ({status}); private output suppressed"));
            }
            if eof {
                // Drop still reaps/kills remaining descendants, if any.
                return Ok(bytes);
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

pub fn validate_diagnostics(bytes: &[u8]) -> Result<()> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid diagnostics JSON")?;
    for name in ["keymint", "keystore2", "injector"] {
        let service = value
            .get(name)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("diagnostics missing {name}"))?;
        let status = service
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("diagnostics invalid {name} status"))?;
        if status != "running" {
            return Err(format!("{name} is {}", single_line(status)));
        }
        if !service
            .get("pid")
            .and_then(Value::as_u64)
            .is_some_and(|pid| pid > 0 && pid <= i32::MAX as u64)
        {
            return Err(format!("diagnostics invalid {name} pid"));
        }
    }
    Ok(())
}

fn local_status_once(paths: &Paths) -> Result<()> {
    let bytes = command_output(
        &paths.keymint(),
        &["--webui-get-runtime-health".into()],
        HEALTH_TIMEOUT,
        STATS_LIMIT,
    )?;
    validate_diagnostics(&bytes)
}

fn confirm_local_health(
    mut probe: impl FnMut() -> Result<()>,
    recheck: impl FnOnce(),
) -> Result<()> {
    if probe().is_ok() {
        return Ok(());
    }
    recheck();
    // A failed first sample must not become a permanent error after a normal
    // keystore restart. A second failure remains authoritative, never cached OK.
    probe()
}

fn local_status(paths: &Paths) -> Result<()> {
    confirm_local_health(|| local_status_once(paths), || thread::sleep(HEALTH_RECHECK_DELAY))
}

pub fn validate_stats(bytes: &[u8]) -> Result<()> {
    if bytes.len() > STATS_LIMIT {
        return Err("stats response exceeds 8192 bytes".into());
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "cloud stats returned invalid JSON")?;
    for field in ["devices", "peak", "keybox_distinct", "version_code"] {
        if value.get(field).and_then(Value::as_u64).is_none() {
            return Err(format!("cloud stats invalid {field}"));
        }
    }
    if value.get("product").and_then(Value::as_str) != Some("OhMyKeymint-R1") {
        return Err("cloud stats product mismatch".into());
    }
    for field in ["version", "updated_at"] {
        if !value.get(field).and_then(Value::as_str).is_some_and(|s| !s.is_empty()) {
            return Err(format!("cloud stats invalid {field}"));
        }
    }
    Ok(())
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .root_certs(ureq::tls::RootCerts::WebPki)
        .disable_verification(false)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .https_only(true)
        .proxy(None)
        .max_redirects(0)
        .max_response_header_size(8 * 1024)
        .timeout_global(Some(Duration::from_secs(30)))
        .timeout_resolve(Some(Duration::from_secs(5)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(10)))
        .timeout_recv_body(Some(Duration::from_secs(15)))
        .user_agent("r1-helper")
        .build()
        .into()
}

fn get(
    client: &ureq::Agent,
    url: &str,
    validators: &Validators,
) -> Result<ureq::http::Response<ureq::Body>> {
    let mut request = client.get(url);
    if let Some(etag) = &validators.etag {
        request = request.header("If-None-Match", etag);
    }
    if let Some(modified) = &validators.last_modified {
        request = request.header("If-Modified-Since", modified);
    }
    request.call().map_err(|_| {
        "cloud HTTPS request failed (network, certificate, HTTP error or timeout)".into()
    })
}

fn validate_response(
    status: u16,
    headers: &ureq::http::HeaderMap,
    conditional: bool,
) -> Result<()> {
    match status {
        200 => {
            if headers.contains_key("content-encoding") {
                return Err("encoded cloud responses are unsupported".into());
            }
            Ok(())
        }
        304 if conditional => Ok(()),
        304 => Err("cloud returned 304 without a matching local cache".into()),
        _ => Err("cloud returned unexpected HTTP status".into()),
    }
}

fn cloud_stats() -> Result<()> {
    let mut response = get(&agent(), STATS_URL, &Validators::default())?;
    validate_response(response.status().as_u16(), response.headers(), false)?;
    let bytes = read_limited(response.body_mut().as_reader(), STATS_LIMIT)?;
    validate_stats(&bytes)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Validators {
    etag: Option<String>,
    last_modified: Option<String>,
}

impl Validators {
    fn from_headers(headers: &ureq::http::HeaderMap) -> Self {
        let field =
            |name| headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_string);
        let mut validators = Self { etag: field("etag"), last_modified: field("last-modified") };
        validators.sanitize();
        validators
    }

    fn sanitize(&mut self) {
        let valid = |value: &str| {
            !value.is_empty()
                && value.len() <= 512
                && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        };
        self.etag = self.etag.take().filter(|value| {
            let quoted = value.strip_prefix("W/").unwrap_or(value);
            valid(value)
                && quoted.len() >= 2
                && quoted.starts_with('"')
                && quoted.ends_with('"')
                && quoted.as_bytes()[1..quoted.len() - 1]
                    .iter()
                    .all(|byte| *byte == 0x21 || (0x23..=0x7e).contains(byte))
        });
        self.last_modified = self.last_modified.take().filter(|value| valid(value));
    }

    fn present(&self) -> bool {
        self.etag.is_some() || self.last_modified.is_some()
    }

    fn merge(&mut self, newer: Self) {
        if newer.etag.is_some() {
            self.etag = newer.etag;
        }
        if newer.last_modified.is_some() {
            self.last_modified = newer.last_modified;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum KeyboxUpdate {
    #[default]
    Pending,
    Updated,
    Unchanged,
    Skipped,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckMode {
    Manual,
    Startup,
    Scheduled,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct CloudObservation {
    checked_at: u64,
    boot_id: Option<String>,
    connected: bool,
    reason: String,
    next_check_at: u64,
    failures: u32,
    keybox: KeyboxUpdate,
    keybox_reason: String,
    keybox_checked_at: u64,
    // Only successful installation/byte equality may advance this cache.
    installed_sha256: Option<String>,
    validators: Validators,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn current_boot_id() -> Option<String> {
    let contents = read_file(Path::new("/proc/sys/kernel/random/boot_id"), 64).ok()?;
    let value = std::str::from_utf8(&contents).ok()?.trim();
    (value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        }))
    .then(|| value.to_ascii_lowercase())
}

/// Read `cloud_interval_minutes` from the module's `r1-cloud.json`.
///
/// The WebUI writes this file, so the user can change the polling period without
/// a new package. Anything unreadable, malformed or out of range falls back to
/// the default, because a bad number must never stop the poller.
fn cloud_interval(paths: &Paths) -> Duration {
    let minutes = read_file(&paths.module.join("r1-cloud.json"), STATS_LIMIT)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value.get("cloud_interval_minutes").and_then(Value::as_u64))
        .filter(|minutes| *minutes >= 1 && *minutes <= MAX_CLOUD_INTERVAL_MINUTES)
        .unwrap_or(DEFAULT_CLOUD_INTERVAL_MINUTES);
    Duration::from_secs(minutes * 60)
}

fn retry_delay(failures: u32, interval: Duration) -> Duration {
    let multiplier = 1u64 << failures.saturating_sub(1).min(5);
    Duration::from_secs((CLOUD_RETRY_MIN.as_secs() * multiplier).min(interval.as_secs()))
}

impl CloudObservation {
    fn fresh(&self, interval: Duration) -> bool {
        self.fresh_at(now(), current_boot_id().as_deref(), interval)
    }

    fn fresh_at(&self, timestamp: u64, boot_id: Option<&str>, interval: Duration) -> bool {
        self.checked_at > 0
            && self.boot_id.as_deref().zip(boot_id).is_some_and(|(saved, current)| saved == current)
            && timestamp.checked_sub(self.checked_at).is_some_and(|age| age <= interval.as_secs())
    }

    fn due(&self, timestamp: u64, interval: Duration) -> bool {
        self.checked_at == 0
            || timestamp < self.checked_at
            || timestamp >= self.next_check_at
            || self.next_check_at.saturating_sub(timestamp) > interval.as_secs()
    }

    fn should_check(&self, mode: CheckMode, timestamp: u64, interval: Duration) -> bool {
        mode == CheckMode::Manual
            || (mode == CheckMode::Startup && self.failures == 0)
            || self.due(timestamp, interval)
    }

    fn finish(&mut self, timestamp: u64, success: bool, interval: Duration) {
        self.checked_at = timestamp;
        self.failures = if success { 0 } else { self.failures.saturating_add(1) };
        let delay = if success { interval } else { retry_delay(self.failures, interval) };
        self.next_check_at = timestamp.saturating_add(delay.as_secs());
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        let bytes = serde_json::to_vec(self).map_err(|_| "cannot encode cloud observation")?;
        atomic_write(&paths.data.join("cloud.json"), &bytes, 0o600)
    }

    fn load(paths: &Paths) -> Self {
        let mut observation = read_file(&paths.data.join("cloud.json"), STATS_LIMIT)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .unwrap_or_default();
        observation.validators.sanitize();
        if !observation.installed_sha256.as_deref().is_some_and(valid_sha256) {
            observation.installed_sha256 = None;
            observation.validators = Validators::default();
        }
        observation
    }
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub ok: bool,
    pub reason: String,
    pub cloud_connected: bool,
    pub cloud_phase: String,
    pub phase: String,
    pub cloud_checked_at: u64,
    pub cloud_reason: String,
    pub keybox_update: String,
    pub keybox_reason: String,
    pub keybox_checked_at: u64,
    pub next_cloud_check_at: u64,
}

fn status(local: Result<()>, cloud: &CloudObservation, interval: Duration) -> Status {
    let ok = local.is_ok();
    let reason = local.err().map(|reason| single_line(&reason)).unwrap_or_default();
    let fresh = cloud.fresh(interval);
    let cloud_connected = cloud.connected && fresh;
    let cloud_phase = if !fresh {
        "pending"
    } else if cloud.connected {
        "connected"
    } else {
        "offline"
    };
    let phase = if !ok {
        "failed"
    } else if cloud_phase == "pending" {
        "checking"
    } else if cloud_connected {
        "running"
    } else {
        "degraded"
    };
    let cloud_reason = if cloud_connected {
        String::new()
    } else if fresh && !cloud.reason.is_empty() {
        single_line(&cloud.reason)
    } else if cloud_phase == "offline" {
        "cloud connection failed; retry scheduled".into()
    } else {
        "cloud check pending for this boot".into()
    };
    let keybox_update = match cloud.keybox {
        KeyboxUpdate::Pending => "pending",
        KeyboxUpdate::Updated => "updated",
        KeyboxUpdate::Unchanged => "unchanged",
        KeyboxUpdate::Skipped => "skipped",
        KeyboxUpdate::Failed => "failed",
    }
    .into();
    Status {
        ok,
        reason: if !ok && reason.is_empty() { "local diagnostics failed".into() } else { reason },
        cloud_connected,
        cloud_phase: cloud_phase.into(),
        phase: phase.into(),
        cloud_checked_at: cloud.checked_at,
        cloud_reason,
        keybox_update,
        keybox_reason: single_line(&cloud.keybox_reason),
        keybox_checked_at: cloud.keybox_checked_at,
        next_cloud_check_at: cloud.next_check_at,
    }
}

fn startup_wait(local: &Result<()>, elapsed: Duration) -> bool {
    local.is_err() && elapsed < STARTUP_GRACE
}

#[derive(Default)]
struct HealthStability {
    ready_seen: bool,
    failure_since: Option<Duration>,
}

impl HealthStability {
    fn checking(&mut self, local: &Result<()>, elapsed: Duration) -> bool {
        if local.is_ok() {
            self.ready_seen = true;
            self.failure_since = None;
            return false;
        }
        let since = *self.failure_since.get_or_insert(elapsed);
        if !self.ready_seen {
            return startup_wait(local, elapsed);
        }
        elapsed.saturating_sub(since) < Duration::from_secs(30)
    }
}

#[derive(Deserialize)]
pub struct BuildInfo {
    pub version: String,
    pub hash: String,
    pub variant: String,
}

impl BuildInfo {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let info: Self = serde_json::from_slice(bytes).map_err(|_| "invalid r1-build.json")?;
        if info.version.is_empty()
            || info.version.len() > 80
            || !info.version.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_+".contains(&b))
            || !(7..=64).contains(&info.hash.len())
            || !info.hash.bytes().all(|b| b.is_ascii_hexdigit())
            || !matches!(info.variant.as_str(), "full" | "no-soter" | "lite")
        {
            return Err("invalid r1-build.json version/hash/variant".into());
        }
        Ok(info)
    }
}

pub enum DisplayState<'a> {
    Waiting(&'a str),
    Checked(&'a Status),
}

pub fn description(build: &BuildInfo, state: DisplayState<'_>) -> String {
    let hash = &build.hash[build.hash.len() - 7..];
    match state {
        DisplayState::Waiting(reason) => format!(
            "OMK-R1-\u{ff08}{}\u{ff09}-\u{ff08}{hash}\u{ff09}\u{542f}\u{52a8}/\u{68c0}\u{67e5}\u{4e2d}\u{3002}{}",
            build.version,
            single_line(reason)
        ),
        DisplayState::Checked(status) if status.ok && status.cloud_connected => format!(
            "\u{2705} OMK-R1-\u{ff08}{}\u{ff09}-\u{ff08}{hash}\u{ff09}\u{5df2}\u{6210}\u{529f}\u{8fd0}\u{884c}\u{3002}\u{5df2}\u{6210}\u{529f}\u{8fde}\u{63a5}\u{4e91}\u{7aef}\u{3002}\u{7b49}\u{7740}\u{88ab}\u{6211}\u{683c}\u{4f60}\u{673a}\u{673a}\u{5427}\u{1f604} \u{3010}\u{672c}\u{6a21}\u{5757}\u{7531}\u{3010}\u{6d45}\u{7b11}\u{5450}\u{3011}\u{4e8c}\u{6539}\u{81ea}\u{7528}\u{ff0c}\u{5c5e}\u{514d}\u{8d39}\u{6a21}\u{5757}\u{ff0c}\u{5012}\u{5356}\u{53ef}\u{803b}\u{3002}\u{3011}",
            build.version
        ),
        DisplayState::Checked(status) if status.ok && status.cloud_phase == "pending" => format!(
            "OMK-R1-\u{ff08}{}\u{ff09}-\u{ff08}{hash}\u{ff09}\u{8fd0}\u{884c}\u{6b63}\u{5e38}\u{ff0c}\u{4e91}\u{7aef}\u{68c0}\u{67e5}\u{4e2d}\u{3002}{}",
            build.version,
            single_line(&status.cloud_reason)
        ),
        DisplayState::Checked(status) => {
            let mut reasons = Vec::new();
            if !status.ok {
                let reason = single_line(&status.reason);
                reasons.push(if reason.is_empty() { "local diagnostics failed".into() } else { reason });
            }
            if !status.cloud_connected {
                let reason = single_line(&status.cloud_reason);
                reasons.push(format!(
                    "\u{4e91}\u{7aef}\u{8fde}\u{63a5}\u{5931}\u{8d25}\u{ff1a}{}",
                    if reason.is_empty() { "cloud check pending" } else { &reason }
                ));
            }
            format!(
                "\u{274c} OMK-R1 \u{8fd0}\u{884c}\u{5931}\u{8d25}\u{3002}\u{ff08}{}\u{ff09}\u{4f60}\u{5378}\u{8f7d}\u{5427}\u{3002}",
                reasons.join("; ")
            )
        }
    }
}

pub fn replace_description(contents: &str, description: &str) -> String {
    let description = clean_line(description, usize::MAX);
    let mut output = String::new();
    let mut replaced = false;
    for line in contents.split_inclusive('\n') {
        if line.trim_start().starts_with("description=") {
            if !replaced {
                output.push_str(&format!("description={description}\n"));
                replaced = true;
            }
        } else {
            output.push_str(line);
        }
    }
    if !replaced {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&format!("description={description}\n"));
    }
    output
}

struct TempFile {
    path: PathBuf,
    file: File,
}

impl TempFile {
    fn create(directory: &Path, prefix: &str) -> Result<Self> {
        for _ in 0..32 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!(".{prefix}-{}-{sequence}.tmp", std::process::id()));
            match OpenOptions::new().write(true).read(true).create_new(true).mode(0o600).open(&path)
            {
                Ok(file) => {
                    let temp = Self { path, file };
                    temp.file
                        .set_permissions(fs::Permissions::from_mode(0o600))
                        .map_err(|_| "cannot secure temporary file")?;
                    return Ok(temp);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("cannot create private temporary file".into()),
            }
        }
        Err("temporary file name collision".into())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("r1-helper: temporary file cleanup failed");
            }
        }
    }
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().ok_or("invalid destination directory")?;
    let mut temp = TempFile::create(parent, "r1-write")?;
    temp.file.write_all(bytes).map_err(|_| "cannot write temporary file")?;
    temp.file
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|_| "cannot preserve file mode")?;
    temp.file.sync_all().map_err(|_| "cannot sync temporary file")?;
    fs::rename(&temp.path, path).map_err(|_| "cannot atomically replace file")?;
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| "cannot sync destination directory".to_string())
}

struct Lock(File);

impl Lock {
    fn acquire(path: &Path, timeout: Duration) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| "cannot open R1 lock")?;
        let start = Instant::now();
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err("cannot acquire R1 lock".into());
            }
            if start.elapsed() >= timeout {
                return Err("another R1 operation is running".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn update_description(paths: &Paths, build: &BuildInfo, state: DisplayState<'_>) -> Result<()> {
    let _lock = Lock::acquire(&paths.data.join("description.lock"), Duration::from_secs(3))?;
    let path = paths.module.join("module.prop");
    let metadata = fs::symlink_metadata(&path).map_err(|_| "module.prop is unavailable")?;
    if !metadata.is_file() {
        return Err("module.prop must be a regular file".into());
    }
    let original =
        String::from_utf8(read_file(&path, 64 * 1024)?).map_err(|_| "module.prop is not UTF-8")?;
    let updated = replace_description(&original, &description(build, state));
    if updated != original {
        atomic_write(&path, updated.as_bytes(), metadata.permissions().mode())?;
    }
    Ok(())
}

fn valid_sha256(expected: &str) -> bool {
    expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn verify_keybox(bytes: &[u8], expected: &str) -> Result<()> {
    if bytes.is_empty() || bytes.len() > KEYBOX_LIMIT {
        return Err("keybox response empty or exceeds 65536 bytes".into());
    }
    if !valid_sha256(expected) {
        return Err("missing or invalid X-Keybox-Sha256 header".into());
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err("keybox SHA-256 mismatch".into());
    }
    Ok(())
}

struct KeyboxResponse {
    status: u16,
    headers: ureq::http::HeaderMap,
    bytes: Vec<u8>,
}

fn download_keybox(
    client: &ureq::Agent,
    url: &str,
    validators: &Validators,
) -> Result<KeyboxResponse> {
    let mut response = get(client, url, validators)?;
    let status = response.status().as_u16();
    validate_response(status, response.headers(), validators.present())?;
    let bytes = if status == 200 {
        read_limited(response.body_mut().as_reader(), KEYBOX_LIMIT)?
    } else {
        Vec::new()
    };
    Ok(KeyboxResponse { status, headers: response.headers().clone(), bytes })
}

// Production keeps fixed HTTPS sources and native import; tests use only fixtures.
trait UpdateBackend {
    fn stats(&mut self) -> Result<()>;
    fn download(&mut self, validators: &Validators) -> Result<KeyboxResponse>;
    fn local_keybox(&self) -> Result<Vec<u8>>;
    fn install(&mut self, bytes: &[u8]) -> Result<()>;
    // Bytes of the keybox shipped with the module. Used only to tell a
    // user-replaced keybox apart from the bundled seed. Tests that do not
    // care about that distinction keep the default (unprovable).
    fn bundled_keybox(&self) -> Result<Vec<u8>> {
        Err("bundled keybox is unavailable".into())
    }
}

struct NativeBackend<'a>(&'a Paths);

impl UpdateBackend for NativeBackend<'_> {
    fn stats(&mut self) -> Result<()> {
        cloud_stats()
    }

    fn download(&mut self, validators: &Validators) -> Result<KeyboxResponse> {
        download_keybox(&agent(), KEYBOX_URL, validators)
    }

    fn local_keybox(&self) -> Result<Vec<u8>> {
        read_file(Path::new(KEYBOX_PATH), KEYBOX_LIMIT)
    }

    fn bundled_keybox(&self) -> Result<Vec<u8>> {
        read_file(&self.0.module.join("keybox.xml"), KEYBOX_LIMIT)
    }

    fn install(&mut self, bytes: &[u8]) -> Result<()> {
        let payload = STANDARD.encode(bytes);
        let mut args = vec!["--webui-install-keybox".to_string()];
        for chunk in payload.as_bytes().chunks(48 * 1024) {
            args.push(std::str::from_utf8(chunk).map_err(|_| "cannot encode keybox")?.to_string());
        }
        let output = command_output(&self.0.keymint(), &args, COMMAND_TIMEOUT, 1024)?;
        if output != b"ok\n" && output != b"ok" {
            return Err(
                "native keybox import returned unexpected acknowledgement; verify installed state"
                    .into(),
            );
        }
        // The native importer validates before atomic replacement. Its watcher reloads.
        Ok(())
    }
}

fn local_matches(backend: &impl UpdateBackend, digest: &str) -> bool {
    backend.local_keybox().is_ok_and(|bytes| verify_keybox(&bytes, digest).is_ok())
}

// A user-supplied keybox (self-signed or manually replaced) is byte-distinct
// from both the bundled seed and the last cloud-installed bytes. Auto-update
// must leave it untouched. Bundled-seed and cloud-managed installs still update.
fn preserves_custom_keybox(cloud: &CloudObservation, backend: &impl UpdateBackend) -> bool {
    let Ok(local) = backend.local_keybox() else { return false };
    if local.is_empty() || local.len() > KEYBOX_LIMIT {
        return false;
    }
    let local_digest = format!("{:x}", Sha256::digest(&local));
    if cloud.installed_sha256.as_deref().is_some_and(|digest| {
        digest.eq_ignore_ascii_case(&local_digest)
    }) {
        return false;
    }
    match backend.bundled_keybox() {
        Ok(bundled) => local != bundled,
        // Without the bundled reference we cannot prove the file is custom.
        Err(_) => false,
    }
}

fn update_keybox(
    paths: &Paths,
    cloud: &mut CloudObservation,
    backend: &mut impl UpdateBackend,
) -> Result<bool> {
    // Never condition a request on a failed import or a changed/missing local file.
    let validators = match cloud.installed_sha256.as_deref() {
        Some(digest) if local_matches(backend, digest) => cloud.validators.clone(),
        _ => Validators::default(),
    };
    let response = backend.download(&validators)?;
    validate_response(response.status, &response.headers, validators.present())?;
    let newer = Validators::from_headers(&response.headers);
    if response.status == 304 {
        let digest = cloud.installed_sha256.as_deref().ok_or("keybox cache is unavailable")?;
        if !local_matches(backend, digest) {
            return Err("local keybox changed during conditional download; retry required".into());
        }
        if let Some(header) = response.headers.get("X-Keybox-Sha256") {
            let expected = header.to_str().map_err(|_| "invalid X-Keybox-Sha256 header")?;
            if !valid_sha256(expected) || !expected.eq_ignore_ascii_case(digest) {
                return Err("keybox SHA-256 mismatch on 304 response".into());
            }
        }
        cloud.validators.merge(newer);
        return Ok(true);
    }
    let expected = response
        .headers
        .get("X-Keybox-Sha256")
        .and_then(|header| header.to_str().ok())
        .ok_or("missing or invalid X-Keybox-Sha256 header")?;
    verify_keybox(&response.bytes, expected)?;
    let unchanged = backend.local_keybox().is_ok_and(|local| local == response.bytes);
    if !unchanged {
        let mut temp = TempFile::create(&paths.data, "keybox")?;
        temp.file.write_all(&response.bytes).map_err(|_| "cannot stage cloud keybox")?;
        temp.file.sync_all().map_err(|_| "cannot sync staged keybox")?;
        let staged = read_file(&temp.path, KEYBOX_LIMIT)?;
        verify_keybox(&staged, expected)?;
        backend.install(&staged)?;
        // A missing/changed file after acknowledgement must not advance the cache.
        if !local_matches(backend, expected) {
            return Err(
                "native keybox import did not confirm installed bytes; retry required".into()
            );
        }
    }
    cloud.installed_sha256 = Some(expected.to_ascii_lowercase());
    cloud.validators = newer;
    Ok(unchanged)
}

// One update lock covers network, native import, retry schedule and atomic state.
// None means another updater won the lock or the persisted schedule is not due.
fn cloud_check(
    paths: &Paths,
    mode: CheckMode,
    backend: &mut impl UpdateBackend,
    interval: Duration,
) -> Result<Option<bool>> {
    let _lock = match Lock::acquire(&paths.data.join("update.lock"), Duration::ZERO) {
        Ok(lock) => lock,
        Err(reason) if mode != CheckMode::Manual && reason == "another R1 operation is running" => {
            return Ok(None);
        }
        Err(reason) => return Err(reason),
    };
    let mut cloud = CloudObservation::load(paths);
    if !cloud.should_check(mode, now(), interval) {
        return Ok(None);
    }
    let stats = backend.stats();
    // Automatic checks never replace a user-supplied keybox. The explicit
    // manual action still imports the cloud keybox on request.
    let skip_custom =
        mode != CheckMode::Manual && preserves_custom_keybox(&cloud, backend);
    let outcome =
        if skip_custom { Ok(true) } else { update_keybox(paths, &mut cloud, backend) };
    let timestamp = now();
    // Either endpoint can prove a verified connection. A stats-schema outage
    // must not erase a successful, validated keybox response (or vice versa).
    cloud.connected = stats.is_ok() || outcome.is_ok();
    cloud.reason = if cloud.connected {
        String::new()
    } else {
        format!(
            "stats: {}; keybox: {}",
            stats.as_ref().err().map(String::as_str).unwrap_or("not checked"),
            outcome.as_ref().err().map(String::as_str).unwrap_or("not checked")
        )
    };
    cloud.boot_id = current_boot_id();
    cloud.keybox = if skip_custom {
        KeyboxUpdate::Skipped
    } else {
        match &outcome {
            Ok(true) => KeyboxUpdate::Unchanged,
            Ok(false) => KeyboxUpdate::Updated,
            Err(_) => KeyboxUpdate::Failed,
        }
    };
    cloud.keybox_reason = if skip_custom {
        "custom keybox preserved; cloud auto-update skipped".into()
    } else {
        outcome.as_ref().err().cloned().unwrap_or_default()
    };
    if outcome.is_ok() {
        cloud.keybox_checked_at = timestamp;
    }
    cloud.finish(timestamp, cloud.connected && outcome.is_ok(), interval);
    // A state write failure returns an error, never a successful update receipt.
    cloud.save(paths)?;
    outcome.map(Some)
}

fn refresh_description(paths: &Paths, build: &BuildInfo) -> Result<()> {
    let interval = cloud_interval(paths);
    let checked = status(local_status(paths), &CloudObservation::load(paths), interval);
    update_description(paths, build, DisplayState::Checked(&checked))
}

fn cloud_update(paths: &Paths, build: &BuildInfo) -> Result<bool> {
    let interval = cloud_interval(paths);
    let outcome = cloud_check(paths, CheckMode::Manual, &mut NativeBackend(paths), interval);
    if let Err(reason) = refresh_description(paths, build) {
        eprintln!("r1-helper: status refresh failed: {reason}");
    }
    outcome?.ok_or_else(|| "cloud update is already running".into())
}

fn monitor(paths: &Paths, build: &BuildInfo) -> Result<()> {
    let _monitor = Lock::acquire(&paths.data.join("monitor.lock"), Duration::ZERO)?;
    // Read once per process: the interval only changes when the user edits the
    // setting, and a fresh helper run picks that up.
    let interval = cloud_interval(paths);
    update_description(paths, build, DisplayState::Waiting("waiting for Android boot completion"))?;
    let boot_started = Instant::now();
    loop {
        let boot = command_output(
            Path::new("/system/bin/getprop"),
            &["sys.boot_completed".into()],
            Duration::from_secs(3),
            128,
        );
        match boot {
            Ok(bytes) if bytes == b"1\n" || bytes == b"1" => break,
            Ok(_) => thread::sleep(Duration::from_secs(5)),
            Err(reason) => {
                if boot_started.elapsed() < STARTUP_GRACE {
                    update_description(paths, build, DisplayState::Waiting(&reason))?;
                } else {
                    let checked = status(Err(reason), &CloudObservation::load(paths), interval);
                    update_description(paths, build, DisplayState::Checked(&checked))?;
                }
                thread::sleep(Duration::from_secs(5));
            }
        }
    }
    let boot_ready = Instant::now();
    let mut stability = HealthStability::default();
    let mut next_cloud = Instant::now();
    let mut mode = CheckMode::Startup;
    loop {
        let cycle = Instant::now();
        let mut local = local_status(paths);
        if local.is_ok() && cycle >= next_cloud {
            let checked = cloud_check(paths, mode, &mut NativeBackend(paths), interval);
            if !matches!(checked, Ok(None)) {
                mode = CheckMode::Scheduled;
            }
            if let Err(reason) = &checked {
                eprintln!("r1-helper: cloud update deferred: {}", single_line(reason));
            }
            let cloud = CloudObservation::load(paths);
            let delay = if checked.is_err() && cloud.due(now(), interval) {
                // Also back off if the retry schedule could not be persisted.
                CLOUD_RETRY_MIN
            } else if cloud.due(now(), interval) {
                // A concurrent manual update holds the lock; no network ran here.
                LOCAL_INTERVAL
            } else {
                Duration::from_secs(cloud.next_check_at.saturating_sub(now()))
            };
            next_cloud = Instant::now() + delay;
            local = local_status(paths);
        }
        let health_checking = stability.checking(&local, boot_ready.elapsed());
        if health_checking {
            update_description(
                paths,
                build,
                DisplayState::Waiting(
                    local.as_ref().err().map(String::as_str).unwrap_or("checking runtime health"),
                ),
            )?;
        } else {
            let checked = status(local, &CloudObservation::load(paths), interval);
            update_description(paths, build, DisplayState::Checked(&checked))?;
        }
        let interval = if health_checking { Duration::from_secs(5) } else { LOCAL_INTERVAL };
        thread::sleep(interval.saturating_sub(cycle.elapsed()));
    }
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<()> {
    let command = args.next().ok_or("usage: r1-helper status|cloud-update|monitor")?;
    if args.next().is_some() || !matches!(command.as_str(), "status" | "cloud-update" | "monitor") {
        return Err("usage: r1-helper status|cloud-update|monitor".into());
    }
    let paths = Paths::default();
    if command == "status" {
        let interval = cloud_interval(&paths);
        let checked = status(local_status(&paths), &CloudObservation::load(&paths), interval);
        println!("{}", serde_json::to_string(&checked).map_err(|_| "cannot encode status")?);
        return Ok(());
    }
    paths.prepare()?;
    let build = BuildInfo::parse(&read_file(&paths.module.join("r1-build.json"), STATS_LIMIT)?)?;
    match command.as_str() {
        "cloud-update" => {
            let unchanged = cloud_update(&paths, &build)?;
            println!("{{\"ok\":true,\"unchanged\":{unchanged}}}");
            Ok(())
        }
        "monitor" => monitor(&paths, &build),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
