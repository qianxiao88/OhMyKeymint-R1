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
const CLOUD_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const LOCAL_INTERVAL: Duration = Duration::from_secs(60);
const STARTUP_GRACE: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
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

fn local_status(paths: &Paths) -> Result<()> {
    let bytes = command_output(
        &paths.keymint(),
        &["--webui-get-diagnostics".into()],
        COMMAND_TIMEOUT,
        STATS_LIMIT,
    )?;
    validate_diagnostics(&bytes)
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

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|_| "cloud HTTPS request failed (network, certificate, HTTP error or timeout)")?;
    if response.status().as_u16() != 200 {
        return Err("cloud returned unexpected HTTP status".into());
    }
    if response.headers().contains_key("content-encoding") {
        return Err("encoded cloud responses are unsupported".into());
    }
    Ok(response)
}

fn cloud_stats() -> Result<()> {
    let mut response = get(STATS_URL)?;
    let bytes = read_limited(response.body_mut().as_reader(), STATS_LIMIT)?;
    validate_stats(&bytes)
}

#[derive(Serialize, Deserialize)]
struct CloudObservation {
    checked_at: u64,
    connected: bool,
    reason: String,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

impl CloudObservation {
    fn check() -> Self {
        match cloud_stats() {
            Ok(()) => Self { checked_at: now(), connected: true, reason: String::new() },
            Err(reason) => Self { checked_at: now(), connected: false, reason },
        }
    }

    fn fresh(&self) -> bool {
        now().checked_sub(self.checked_at).is_some_and(|age| age < CLOUD_INTERVAL.as_secs())
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        let bytes = serde_json::to_vec(self).map_err(|_| "cannot encode cloud observation")?;
        atomic_write(&paths.data.join("cloud.json"), &bytes, 0o600)
    }

    fn load(paths: &Paths) -> Self {
        read_file(&paths.data.join("cloud.json"), STATS_LIMIT)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .filter(Self::fresh)
            .unwrap_or(Self {
                checked_at: 0,
                connected: false,
                reason: "cloud stats awaiting verification".into(),
            })
    }
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub ok: bool,
    pub reason: String,
    pub cloud_connected: bool,
}

fn status(local: Result<()>, cloud: &CloudObservation) -> Status {
    let local_ok = local.is_ok();
    let cloud_connected = cloud.connected && cloud.fresh();
    let mut reason = match local {
        Err(reason) => single_line(&reason),
        Ok(()) if cloud_connected => String::new(),
        Ok(()) if cloud.fresh() => single_line(&cloud.reason),
        Ok(()) => "cloud stats awaiting verification".into(),
    };
    let ok = local_ok && cloud_connected;
    if !ok && reason.is_empty() {
        reason =
            if local_ok { "cloud stats unavailable" } else { "local diagnostics failed" }.into();
    }
    Status { ok, reason, cloud_connected }
}

fn startup_wait(local: &Result<()>, elapsed: Duration) -> bool {
    local.is_err() && elapsed < STARTUP_GRACE
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
    match state {
        DisplayState::Waiting(reason) => format!("OMK-R1 \u{542f}\u{52a8}/\u{68c0}\u{67e5}\u{4e2d}\u{3002}{}", single_line(reason)),
        DisplayState::Checked(status) if status.ok && status.cloud_connected => format!(
            "\u{2705} OMK-R1-\u{ff08}{}\u{ff09}-\u{ff08}{}\u{ff09}\u{5df2}\u{6210}\u{529f}\u{8fd0}\u{884c}\u{3002}\u{5df2}\u{6210}\u{529f}\u{8fde}\u{63a5}\u{4e91}\u{7aef}\u{3002}\u{7b49}\u{7740}\u{88ab}\u{6211}\u{683c}\u{4f60}\u{673a}\u{673a}\u{5427}\u{1f604} \u{3010}\u{672c}\u{6a21}\u{5757}\u{7531}\u{3010}\u{6d45}\u{7b11}\u{5450}\u{3011}\u{4e8c}\u{6539}\u{81ea}\u{7528}\u{ff0c}\u{5c5e}\u{514d}\u{8d39}\u{6a21}\u{5757}\u{ff0c}\u{5012}\u{5356}\u{53ef}\u{803b}\u{3002}\u{3011}",
            build.version, &build.hash[build.hash.len() - 7..]
        ),
        DisplayState::Checked(status) => format!(
            "\u{274c} OMK-R1 \u{8fd0}\u{884c}\u{5931}\u{8d25}\u{3002}\u{ff08}{}\u{ff09}\u{4f60}\u{5378}\u{8f7d}\u{5427}\u{3002}", single_line(&status.reason)
        ),
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

pub fn verify_keybox(bytes: &[u8], expected: &str) -> Result<()> {
    if bytes.is_empty() || bytes.len() > KEYBOX_LIMIT {
        return Err("keybox response empty or exceeds 65536 bytes".into());
    }
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("missing or invalid X-Keybox-Sha256 header".into());
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        return Err("keybox SHA-256 mismatch".into());
    }
    Ok(())
}

fn keybox_unchanged(path: &Path, staged: &[u8]) -> bool {
    read_file(path, KEYBOX_LIMIT).is_ok_and(|local| local == staged)
}

fn cloud_update(paths: &Paths, build: &BuildInfo) -> Result<bool> {
    let _lock = Lock::acquire(&paths.data.join("update.lock"), Duration::ZERO)?;
    update_description(
        paths,
        build,
        DisplayState::Waiting("downloading and validating cloud keybox"),
    )?;
    let mut response = get(KEYBOX_URL)?;
    let expected = response
        .headers()
        .get("X-Keybox-Sha256")
        .and_then(|v| v.to_str().ok())
        .ok_or("missing or invalid X-Keybox-Sha256 header")?
        .to_string();
    let mut temp = TempFile::create(&paths.data, "keybox")?;
    let count = std::io::copy(
        &mut response.body_mut().as_reader().take(KEYBOX_LIMIT as u64 + 1),
        &mut temp.file,
    )
    .map_err(|_| "cannot download keybox to private staging file (I/O or timeout)")?;
    if count > KEYBOX_LIMIT as u64 {
        return Err("keybox response exceeds 65536 bytes".into());
    }
    temp.file.sync_all().map_err(|_| "cannot sync staged keybox")?;
    let staged = read_file(&temp.path, KEYBOX_LIMIT)?;
    verify_keybox(&staged, &expected)?;
    let unchanged = keybox_unchanged(Path::new(KEYBOX_PATH), &staged);
    let payload = STANDARD.encode(&staged);
    let mut args = vec!["--webui-install-keybox".to_string()];
    for chunk in payload.as_bytes().chunks(48 * 1024) {
        args.push(std::str::from_utf8(chunk).map_err(|_| "cannot encode keybox")?.to_string());
    }
    let output = command_output(&paths.keymint(), &args, COMMAND_TIMEOUT, 1024)?;
    if output != b"ok\n" && output != b"ok" {
        return Err(
            "native keybox import returned unexpected acknowledgement; verify installed state"
                .into(),
        );
    }
    drop(temp);
    // Native install validates before replacement; its existing watcher performs reload.
    // Import success stays authoritative even if subsequent status refresh fails.
    let cloud = CloudObservation::check();
    if let Err(reason) = cloud.save(paths) {
        eprintln!("r1-helper: {reason}");
    }
    let checked = status(local_status(paths), &cloud);
    if let Err(reason) = update_description(paths, build, DisplayState::Checked(&checked)) {
        eprintln!("r1-helper: keybox imported; status refresh failed: {reason}");
    }
    Ok(unchanged)
}

fn monitor(paths: &Paths, build: &BuildInfo) -> Result<()> {
    let _monitor = Lock::acquire(&paths.data.join("monitor.lock"), Duration::ZERO)?;
    update_description(paths, build, DisplayState::Waiting("waiting for Android boot completion"))?;
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
                let checked = Status { ok: false, reason: reason.clone(), cloud_connected: false };
                update_description(paths, build, DisplayState::Checked(&checked))?;
                return Err(reason);
            }
        }
    }
    let boot_ready = Instant::now();
    let mut next_cloud = Instant::now();
    loop {
        let cycle = Instant::now();
        let local = local_status(paths);
        if cycle >= next_cloud {
            let cloud = CloudObservation::check();
            cloud.save(paths)?;
            next_cloud =
                Instant::now() + if cloud.connected { CLOUD_INTERVAL } else { LOCAL_INTERVAL };
        }
        if startup_wait(&local, boot_ready.elapsed()) {
            update_description(
                paths,
                build,
                DisplayState::Waiting("waiting for keymint/keystore2/injector"),
            )?;
        } else {
            let checked = status(local, &CloudObservation::load(paths));
            update_description(paths, build, DisplayState::Checked(&checked))?;
        }
        thread::sleep(LOCAL_INTERVAL.saturating_sub(cycle.elapsed()));
    }
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<()> {
    let command = args.next().ok_or("usage: r1-helper status|cloud-update|monitor")?;
    if args.next().is_some() || !matches!(command.as_str(), "status" | "cloud-update" | "monitor") {
        return Err("usage: r1-helper status|cloud-update|monitor".into());
    }
    let paths = Paths::default();
    if command == "status" {
        let checked = status(local_status(&paths), &CloudObservation::load(&paths));
        println!("{}", serde_json::to_string(&checked).map_err(|_| "cannot encode status")?);
        return Ok(());
    }
    paths.prepare()?;
    let build = BuildInfo::parse(&read_file(&paths.module.join("r1-build.json"), STATS_LIMIT)?)?;
    match command.as_str() {
        "cloud-update" => {
            let unchanged = match cloud_update(&paths, &build) {
                Ok(unchanged) => unchanged,
                Err(reason) => {
                    let checked =
                        Status { ok: false, reason: single_line(&reason), cloud_connected: false };
                    if let Err(error) =
                        update_description(&paths, &build, DisplayState::Checked(&checked))
                    {
                        eprintln!("r1-helper: status refresh failed: {error}");
                    }
                    return Err(reason);
                }
            };
            println!("{{\"ok\":true,\"unchanged\":{unchanged}}}");
            Ok(())
        }
        "monitor" => monitor(&paths, &build),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests;
