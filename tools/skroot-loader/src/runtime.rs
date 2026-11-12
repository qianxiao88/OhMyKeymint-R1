use crate::{
    package::{self, fail, Manifest, Result, MODULE_ROOT, STATE_ROOT},
    policy,
};
use std::{
    ffi::c_char,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop_signal(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

struct Lock(File);
impl Lock {
    fn acquire(path: &Path, timeout: Duration) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(fail("invalid lock file"));
        }
        let started = Instant::now();
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            if started.elapsed() >= timeout {
                return Err(fail("loader lock busy"));
            }
            std::thread::sleep(Duration::from_millis(50));
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

fn state() -> Result<PathBuf> {
    let path = PathBuf::from(STATE_ROOT);
    package::create_plain_directory(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

#[cfg(target_os = "android")]
unsafe fn private_from_abi(directory: *const c_char) -> Result<PathBuf> {
    if directory.is_null() || unsafe { libc::strnlen(directory, 4096) } >= 4096 {
        return Err(fail("invalid module directory"));
    }
    let path = PathBuf::from(unsafe { std::ffi::CStr::from_ptr(directory) }.to_str()?);
    validate_private(&path)?;
    Ok(path)
}

fn validate_private(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.components().count() < 4
        || path == Path::new(MODULE_ROOT)
        || path.starts_with(MODULE_ROOT)
        || Path::new(MODULE_ROOT).starts_with(path)
    {
        return Err(fail("invalid SKRoot resource directory"));
    }
    if !path.starts_with("/data")
        || path.starts_with("/data/adb")
        || path.starts_with("/data/misc")
        || path.starts_with("/data/user")
        || path.starts_with("/data/user_de")
        || path.starts_with("/data/data")
    {
        return Err(fail("resource directory is outside the SKRoot private layout"));
    }
    package::plain_path(path)?;
    // The upstream ABI supplies this private directory; its ancestors may
    // contain a root-key prefix. Never log or persist any part of this path.
    if unsafe { libc::geteuid() } != 0 {
        return Err(fail("root execution context required"));
    }
    Ok(())
}

fn prepare(private: &Path) -> Result<Manifest> {
    validate_private(private)?;
    // SKRoot extracts resources as 0777. Restrict the trusted private root first.
    fs::set_permissions(private, fs::Permissions::from_mode(0o700))?;
    let manifest = package::read_manifest(private)?;
    package::verify_payload(private, &manifest)?;
    for entry in &manifest.files {
        fs::set_permissions(
            private.join("payload").join(&entry.path),
            fs::Permissions::from_mode(entry.mode),
        )?;
    }
    policy::preflight(&private.join("payload"))?;
    Ok(manifest)
}

/// # Safety
/// The directory is an upstream ABI NUL-terminated string.
pub unsafe fn install_callback(directory: *const c_char) -> Result<()> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = directory;
        Err(fail("Android callback only"))
    }
    #[cfg(target_os = "android")]
    {
        let private = unsafe { private_from_abi(directory)? };
        let state = state()?;
        let _operation = Lock::acquire(&state.join("operation.lock"), Duration::from_secs(3))?;
        let manifest = match prepare(&private) {
            Ok(manifest) => manifest,
            Err(error) => {
                let _ = status(&state, "blocked", &error.to_string());
                return Err(error);
            }
        };
        let root = Path::new(MODULE_ROOT);
        if package::owned(root)? && package::disabled(root)? {
            return Err(fail("module disabled"));
        }
        fs::write(state.join("stop"), b"update\n")?;
        let installed = (|| {
            let _life = Lock::acquire(&state.join("lifetime.lock"), Duration::from_secs(10))?;
            package::create_plain_directory(
                root.parent().ok_or_else(|| fail("missing module parent"))?,
            )?;
            package::install(&private, root, &manifest)
        })();
        let cleanup = fs::remove_file(state.join("stop"));
        installed?;
        cleanup?;
        status(&state, "installed", "reboot required; services not started by install callback")?;
        Ok(())
    }
}

/// # Safety
/// The directory is an upstream ABI NUL-terminated string.
pub unsafe fn uninstall_callback(directory: *const c_char) -> Result<()> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = directory;
        Err(fail("Android callback only"))
    }
    #[cfg(target_os = "android")]
    {
        let _private = unsafe { private_from_abi(directory)? };
        let root = Path::new(MODULE_ROOT);
        if !package::owned(root)? {
            return Ok(());
        }
        let state = state()?;
        let _operation = Lock::acquire(&state.join("operation.lock"), Duration::from_secs(3))?;
        fs::write(state.join("stop"), b"uninstall\n")?;
        let _life = Lock::acquire(&state.join("lifetime.lock"), Duration::from_secs(10))?;
        package::remove_owned(root)?;
        status(&state, "removed", "shared OMK data and other managers preserved")?;
        Ok(())
    }
}

fn status(state: &Path, phase: &str, reason: &str) -> Result<()> {
    let path = state.join(format!(".status-{}", std::process::id()));
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({"schema":1,"phase":phase,"reason":reason,"variant":crate::VARIANT,"version":crate::VERSION}),
    )?;
    file.flush()?;
    file.sync_all()?;
    fs::rename(&path, state.join("status.json"))?;
    File::open(state)?.sync_all()?;
    Ok(())
}

fn module_path() -> &'static str {
    MODULE_ROOT
}

fn script(root: &Path, name: &str) -> Result<Child> {
    let path = root.join(name);
    package::regular(&path)?;
    let path_env = format!("{}/bin:/system/bin:/system/xbin", module_path());
    Ok(Command::new("/system/bin/sh").arg(path).env_clear().env("PATH", path_env)
        .env("LANG", "C").env("LD_LIBRARY_PATH", "/system/lib64:/apex/com.android.runtime/lib64:/apex/com.android.runtime/lib64/bionic:/vendor/lib64")
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?)
}

fn wait_child(child: &mut Child, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() { Ok(()) } else { Err(fail("module script failed")) };
        }
        if start.elapsed() >= timeout {
            child.kill()?;
            let _ = child.wait();
            return Err(fail("module script timeout"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn terminate_group() {
    let group = unsafe { libc::getpgrp() };
    // Worker is the session/group leader. Only its own spawned descendants share it.
    if group != unsafe { libc::getpid() } {
        return;
    }
    unsafe {
        libc::kill(-group, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(1500));
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            if pid == group {
                continue;
            }
            let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let Some(fields) =
                stat.rsplit_once(')').map(|(_, rest)| rest.split_whitespace().collect::<Vec<_>>())
            else {
                continue;
            };
            if fields.get(2).and_then(|s| s.parse::<i32>().ok()) == Some(group) {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
    unsafe { while libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) > 0 {} }
}

pub fn worker() -> Result<()> {
    if !cfg!(target_os = "android") {
        return Err(fail("Android worker only"));
    }
    if std::env::args().nth(1).as_deref() != Some("boot") {
        return Err(fail("unknown worker action"));
    }
    let mut bytes = Vec::new();
    std::io::stdin().take(4096).read_to_end(&mut bytes)?;
    let private = PathBuf::from(std::str::from_utf8(&bytes)?);
    validate_private(&private)?;
    let state = state()?;
    // Official descriptor recommends 5-10 seconds in the detached child.
    std::thread::sleep(Duration::from_secs(5));
    // Every path acquires operation before lifetime, including callbacks.
    let operation = Lock::acquire(&state.join("operation.lock"), Duration::from_secs(3))?;
    let _life = match Lock::acquire(&state.join("lifetime.lock"), Duration::ZERO) {
        Ok(lock) => lock,
        Err(_) => return Ok(()),
    };
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        libc::signal(libc::SIGTERM, stop_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, stop_signal as *const () as libc::sighandler_t);
    }
    let run = (|| {
        let root = Path::new(MODULE_ROOT);
        if package::disabled(root)? || state.join("stop").try_exists()? {
            return Err(fail("module disabled, removed or update in progress"));
        }
        let manifest = prepare(&private)?;
        package::create_plain_directory(
            root.parent().ok_or_else(|| fail("missing module parent"))?,
        )?;
        package::install(&private, root, &manifest)?;
        for entry in &manifest.files {
            if entry.mode == 0o755 {
                policy::label_executable(&root.join(&entry.path))?;
            }
        }
        policy::preflight(root)?;
        drop(operation);
        status(&state, "starting", "policy verified; starting packaged scripts")?;
        let mut post_fs = script(root, "post-fs-data.sh")?;
        wait_child(&mut post_fs, Duration::from_secs(60))?;
        let mut service = script(root, "service.sh")?;
        status(&state, "dispatched", "service launched; actual health is reported by r1-helper")?;
        loop {
            if STOP.load(Ordering::Relaxed)
                || state.join("stop").try_exists()?
                || package::disabled(root)?
                || !package::owned(root)?
            {
                break;
            }
            if let Some(exit) = service.try_wait()? {
                if !exit.success() {
                    return Err(fail("service script failed"));
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        Ok(())
    })();
    terminate_group();
    match run {
        Ok(()) => status(&state, "stopped", "loader lifecycle stopped"),
        Err(error) => {
            let _ = status(&state, "blocked", &error.to_string());
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests;
