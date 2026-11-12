//! SKRootPro C ABI; all resource and service work runs after exec in the worker.
use std::ffi::{c_char, c_int};

pub mod package;
pub mod policy;
pub mod runtime;

include!(concat!(env!("OUT_DIR"), "/metadata.rs"));

#[cfg(target_os = "android")]
unsafe fn launch(directory: *const c_char, action: &std::ffi::CStr) -> c_int {
    const CAP: usize = 4096;
    if directory.is_null() {
        return -libc::EINVAL;
    }
    // The caller owns a NUL-terminated ABI string. Copy bounded bytes before fork.
    let n = unsafe { libc::strnlen(directory, CAP - 32) };
    if n == 0 || n >= CAP - 32 || unsafe { *directory } != b'/' as c_char {
        return -libc::EINVAL;
    }
    let directory_bytes = unsafe { std::slice::from_raw_parts(directory.cast::<u8>(), n) };
    let Ok(directory_text) = std::str::from_utf8(directory_bytes) else {
        return -libc::EINVAL;
    };
    let path = std::path::Path::new(directory_text);
    if !path.starts_with("/data")
        || path.starts_with("/data/adb")
        || path.starts_with("/data/misc")
        || path.starts_with("/data/user")
        || path.starts_with("/data/user_de")
        || path.starts_with("/data/data")
        || path.components().count() < 4
        || directory_text.split('/').any(|s| s == "." || s == "..")
    {
        return -libc::EINVAL;
    }
    let mut executable = [0_u8; CAP];
    unsafe { std::ptr::copy_nonoverlapping(directory.cast::<u8>(), executable.as_mut_ptr(), n) };
    let suffix = b"/skroot/omk-skroot-worker\0";
    executable[n..n + suffix.len()].copy_from_slice(suffix);
    let mut channel = [-1; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            channel.as_mut_ptr(),
        )
    } != 0
    {
        return -std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO);
    }
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // Only async-signal-safe libc calls until exec. No Rust locks/allocator here.
        unsafe {
            libc::close(channel[1]);
            if libc::setsid() < 0 || libc::dup2(channel[0], 0) < 0 {
                libc::_exit(126);
            }
            if channel[0] != 0 {
                libc::close(channel[0]);
            }
            libc::fcntl(0, libc::F_SETFD, 0);
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if null < 0 {
                libc::_exit(126);
            }
            libc::dup2(null, 1);
            libc::dup2(null, 2);
            if null > 2 {
                libc::close(null);
            }
            // Upstream extraction uses 0777. Repair only this private root and worker.
            if libc::chmod(directory, 0o700) != 0
                || libc::chmod(executable.as_ptr().cast(), 0o700) != 0
            {
                libc::_exit(126);
            }
            let argv = [c"omk-skroot-worker".as_ptr(), action.as_ptr(), std::ptr::null()];
            // Do not inherit root-key-bearing environment, library paths, or manager variables.
            let envp =
                [c"PATH=/system/bin:/system/xbin".as_ptr(), c"LANG=C".as_ptr(), std::ptr::null()];
            libc::execve(executable.as_ptr().cast(), argv.as_ptr(), envp.as_ptr());
            libc::_exit(127);
        }
    }
    unsafe { libc::close(channel[0]) };
    if pid < 0 {
        unsafe { libc::close(channel[1]) };
        return -libc::EAGAIN;
    }
    // Below PIPE_BUF: one bounded write, no path in argv/environment/logs.
    let result = unsafe { libc::send(channel[1], directory.cast(), n, libc::MSG_NOSIGNAL) };
    unsafe { libc::close(channel[1]) };
    if result != n as isize {
        -libc::EIO
    } else {
        0
    }
}

#[cfg(not(target_os = "android"))]
unsafe fn launch(_directory: *const c_char, _action: &std::ffi::CStr) -> c_int {
    -libc::ENOSYS
}

/// SKRootPro early-boot entry. Zero means dispatch accepted, not service health.
///
/// # Safety
/// `module_private_dir` must be a valid NUL-terminated string for this call.
#[no_mangle]
pub unsafe extern "C" fn __skroot_module_main_18fdf393885363712349ef3de5411bd5(
    _root_key: *const c_char,
    module_private_dir: *const c_char,
) -> c_int {
    unsafe { launch(module_private_dir, c"boot") }
}

/// SKRootPro install callback. The worker validates/install synchronously here.
///
/// # Safety
/// The directory and callback must satisfy the upstream C ABI contract.
#[no_mangle]
pub unsafe extern "C" fn __skroot_module_on_install_k6JkibrjUwzXFpshvFuIwdUNRP4fYtLA(
    _root_key: *const c_char,
    module_private_dir: *const c_char,
    callback: Option<unsafe extern "C" fn(*const c_char)>,
) {
    let result = unsafe { runtime::install_callback(module_private_dir) };
    if let Some(callback) = callback {
        unsafe {
            callback(if result.is_ok() {
                c"".as_ptr()
            } else {
                c"OMK SKRootPro package, ownership or preflight check failed; see documented prerequisites".as_ptr()
            });
        }
    }
}

/// SKRootPro uninstall callback; only this loader's marked module is removed.
///
/// # Safety
/// The directory must satisfy the upstream C ABI contract.
#[no_mangle]
pub unsafe extern "C" fn __skroot_module_on_uninstall_epmoso85hw4w1rzjagiuzdmnyqamk7ep(
    _root_key: *const c_char,
    module_private_dir: *const c_char,
) {
    let _ = unsafe { runtime::uninstall_callback(module_private_dir) };
}
