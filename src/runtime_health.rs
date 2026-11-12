//! Read-only process/mapping evidence; this does not validate an injector RPC session.
use kmr_common::consts::KEYSTORE_UID;
use serde::Serialize;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MODULE_ROOT: &str = "/data/adb/modules/oh_my_keymint";
const STATE_ROOT: &str = "/data/adb/omk";

#[derive(Serialize)]
pub(crate) struct ServiceDiagnostic {
    pub(crate) status: &'static str,
    pub(crate) pid: Option<u32>,
}

#[derive(Serialize)]
pub(crate) struct RuntimeHealth {
    pub(crate) keymint: ServiceDiagnostic,
    pub(crate) keystore2: ServiceDiagnostic,
    pub(crate) injector: ServiceDiagnostic,
}

fn service(status: &'static str, pid: Option<u32>) -> ServiceDiagnostic {
    ServiceDiagnostic { status, pid }
}

fn binary_paths(module: &Path, state: &Path, name: &str) -> Vec<PathBuf> {
    vec![
        state.join(name),
        module.join(name),
        module.join("libs/arm64-v8a").join(name),
        module.join("libs/x86_64").join(name),
    ]
}

fn undeleted(path: &Path) -> Option<&str> {
    path.to_str().map(|value| value.strip_suffix(" (deleted)").unwrap_or(value))
}

fn start_time(process: &Path) -> Option<u64> {
    let stat = std::fs::read_to_string(process.join("stat")).ok()?;
    // comm is parenthesized and can itself contain spaces or ')'. Field 22 is starttime.
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

fn effective_uid(process: &Path) -> Option<libc::uid_t> {
    let status = std::fs::read_to_string(process.join("status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:")?.split_whitespace().nth(1)?.parse().ok())
}

fn command_line(process: &Path) -> Option<Vec<String>> {
    let bytes = std::fs::read(process.join("cmdline")).ok()?;
    bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| std::str::from_utf8(arg).ok().map(str::to_owned))
        .collect()
}

struct Mapping<'a> {
    executable: bool,
    device: (u64, u64),
    inode: u64,
    path: &'a str,
    deleted: bool,
}

fn mapping(line: &str) -> Option<Mapping<'_>> {
    let mut fields = line.split_whitespace();
    fields.next()?;
    let executable = fields.next()?.contains('x');
    fields.next()?;
    let (major, minor) = fields.next()?.split_once(':')?;
    let device = (u64::from_str_radix(major, 16).ok()?, u64::from_str_radix(minor, 16).ok()?);
    let inode = fields.next()?.parse().ok()?;
    let path = fields.next()?;
    let deleted = fields.next() == Some("(deleted)");
    Some(Mapping { executable, device, inode, path, deleted })
}

fn mapping_matches_file(mapped: &Mapping<'_>, path: &Path) -> bool {
    if !mapped.executable || mapped.inode == 0 || Path::new(mapped.path) != path {
        return false;
    }
    if mapped.deleted {
        // An atomic hot update can unlink an image which remains mapped in the target.
        return true;
    }
    std::fs::metadata(path).is_ok_and(|metadata| {
        mapped.inode == metadata.ino()
            && mapped.device
                == (libc::major(metadata.dev()) as u64, libc::minor(metadata.dev()) as u64)
    })
}

fn executable_matches(process: &Path, expected: &[PathBuf]) -> bool {
    let exe = process.join("exe");
    let Ok(actual) = std::fs::metadata(&exe) else { return false };
    if expected.iter().any(|path| {
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.dev() == actual.dev() && metadata.ino() == actual.ino())
    }) {
        return true;
    }
    // read_link alone is insufficient after a hot update. Require an executable mapping
    // for this process's actual image as well as an exact deployed (deleted) pathname.
    let Ok(link) = std::fs::read_link(&exe) else { return false };
    let Some(path) = undeleted(&link) else { return false };
    if !link.to_string_lossy().ends_with(" (deleted)")
        || !expected.iter().any(|expected| expected == Path::new(path))
    {
        return false;
    }
    std::fs::read_to_string(process.join("maps")).is_ok_and(|maps| {
        maps.lines().filter_map(mapping).any(|mapped| {
            mapped.executable
                && mapped.deleted
                && mapped.path == path
                && mapped.inode == actual.ino()
                && mapped.device
                    == (libc::major(actual.dev()) as u64, libc::minor(actual.dev()) as u64)
        })
    })
}

fn process_pid(proc_root: &Path, pids: &[u32], name: &str, expected: &[PathBuf]) -> Option<u32> {
    pids.iter().copied().find(|pid| {
        if *pid == std::process::id() {
            return false;
        }
        let process = proc_root.join(pid.to_string());
        let Some(start) = start_time(&process) else { return false };
        let Some(args) = command_line(&process) else { return false };
        let Some(program) = args.first() else { return false };
        if Path::new(program).file_name().and_then(|value| value.to_str()) != Some(name) {
            return false;
        }
        // The forked privileged helper retains the image and argv. Only the service
        // drops to the keystore identity; root startup and helper processes are not ready.
        if name == "keymint" && (args.len() != 1 || effective_uid(&process) != Some(KEYSTORE_UID)) {
            return false;
        }
        let matches = if name == "keystore2" {
            std::fs::read_link(process.join("exe"))
                .is_ok_and(|link| undeleted(&link) == Some("/system/bin/keystore2"))
        } else {
            executable_matches(&process, expected)
        };
        matches && start_time(&process) == Some(start)
    })
}

fn pidfile_process(proc_root: &Path, path: &Path, script: &Path) -> Option<u32> {
    let pid = std::fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()?;
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    let process = proc_root.join(pid.to_string());
    let start = start_time(&process)?;
    let args = command_line(&process)?;
    // Match the actual script argument, not text in another process's arguments.
    let shell = args.first().and_then(|arg| Path::new(arg).file_name())?;
    let script_index = if shell == "sh" { 1 } else { 0 };
    if args.get(script_index).map(Path::new) != Some(script) {
        return None;
    }
    (start_time(&process) == Some(start)).then_some(pid)
}

fn payload_identifier(record: &str, pid: u32) -> Option<&str> {
    let mut fields = record.split_whitespace();
    if fields.next()?.parse::<u32>().ok()? != pid {
        return None;
    }
    let identifier = fields.next()?;
    let random = identifier.strip_prefix("lib")?.strip_suffix(".so")?;
    if fields.next().is_some()
        || random.len() != 32
        || !random.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(identifier)
}

fn injected(maps: &str, record: &str, pid: u32, binaries: &[PathBuf]) -> bool {
    let identifier = payload_identifier(record, pid);
    maps.lines().filter_map(mapping).any(|mapped| {
        if !mapped.executable || mapped.inode == 0 {
            return false;
        }
        if binaries.iter().any(|binary| mapping_matches_file(&mapped, binary)) {
            return true;
        }
        // A random loader/memfd name is evidence only together with the current target
        // PID in daemon-produced state. Exact names prevent substring/stale matches.
        identifier.is_some_and(|identifier| {
            mapped.path.strip_prefix("/memfd:").or_else(|| mapped.path.strip_prefix("memfd:"))
                == Some(identifier)
        })
    })
}

fn inspect(proc_root: &Path, module: &Path, state: &Path) -> RuntimeHealth {
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return RuntimeHealth {
            keymint: service("unknown", None),
            keystore2: service("unknown", None),
            injector: service("unknown", None),
        };
    };
    let mut pids: Vec<u32> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse().ok())
        .collect();
    pids.sort_unstable();
    let keymint = process_pid(proc_root, &pids, "keymint", &binary_paths(module, state, "keymint"));
    let keystore = process_pid(proc_root, &pids, "keystore2", &[]);
    let injector = match keystore {
        None => service("stopped", None),
        Some(pid) => {
            let process = proc_root.join(pid.to_string());
            let start = start_time(&process);
            match std::fs::read_to_string(process.join("maps")) {
                Err(_) => service("unknown", Some(pid)),
                Ok(maps) => {
                    let record =
                        std::fs::read_to_string(state.join("injector.payload")).unwrap_or_default();
                    let mapped =
                        injected(&maps, &record, pid, &binary_paths(module, state, "inject"));
                    if start.is_none()
                        || start_time(&process) != start
                        || process_pid(proc_root, &[pid], "keystore2", &[]) != Some(pid)
                    {
                        service("unknown", None)
                    } else if mapped {
                        service("running", Some(pid))
                    } else if let Some(daemon) = pidfile_process(
                        proc_root,
                        &state.join("injector-daemon.pid"),
                        &module.join("daemon-injector"),
                    ) {
                        service("configured", Some(daemon))
                    } else {
                        service("stopped", None)
                    }
                }
            }
        }
    };
    RuntimeHealth {
        keymint: service(if keymint.is_some() { "running" } else { "stopped" }, keymint),
        keystore2: service(if keystore.is_some() { "running" } else { "stopped" }, keystore),
        injector,
    }
}

pub(crate) fn get() -> RuntimeHealth {
    inspect(Path::new("/proc"), Path::new(MODULE_ROOT), Path::new(STATE_ROOT))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENT: &str = "lib0123456789abcdef0123456789abcdef.so";
    static NEXT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("omk-health-{}-{sequence}", std::process::id()));
            for name in ["proc", "module", "state"] {
                std::fs::create_dir_all(root.join(name)).unwrap();
            }
            Self(root)
        }

        fn process(&self, pid: u32, argv: &[&str], exe: &Path) -> PathBuf {
            let process = self.0.join("proc").join(pid.to_string());
            std::fs::create_dir_all(&process).unwrap();
            std::fs::write(process.join("cmdline"), format!("{}\0", argv.join("\0"))).unwrap();
            std::fs::write(
                process.join("status"),
                format!("Uid:\t{KEYSTORE_UID}\t{KEYSTORE_UID}\t{KEYSTORE_UID}\t{KEYSTORE_UID}\n"),
            )
            .unwrap();
            std::fs::write(
                process.join("stat"),
                format!("{pid} (comm with ) space) S {} 12345\n", ["0"; 18].join(" ")),
            )
            .unwrap();
            std::os::unix::fs::symlink(exe, process.join("exe")).unwrap();
            process
        }

        fn health(&self) -> RuntimeHealth {
            inspect(&self.0.join("proc"), &self.0.join("module"), &self.0.join("state"))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn runtime_requires_target_mapping_and_reports_unknown_maps() {
        let fixture = Fixture::new();
        let process =
            fixture.process(72001, &["/system/bin/keystore2"], Path::new("/system/bin/keystore2"));
        let unknown = fixture.health();
        assert_eq!(unknown.keystore2.pid, Some(72001));
        assert_eq!(unknown.injector.status, "unknown");

        std::fs::write(process.join("maps"), "").unwrap();
        let script = fixture.0.join("module/daemon-injector");
        fixture.process(
            72002,
            &["/system/bin/sh", script.to_str().unwrap()],
            Path::new("/system/bin/sh"),
        );
        std::fs::write(fixture.0.join("state/injector-daemon.pid"), "72002\n").unwrap();
        assert_eq!(fixture.health().injector.status, "configured");

        std::fs::write(fixture.0.join("state/injector.payload"), format!("72000 {IDENT}\n"))
            .unwrap();
        std::fs::write(
            process.join("maps"),
            format!("1000-2000 r-xp 0000 00:01 42 /memfd:{IDENT} (deleted)\n"),
        )
        .unwrap();
        assert_eq!(fixture.health().injector.status, "configured");
        std::fs::write(fixture.0.join("state/injector.payload"), format!("72001 {IDENT}\n"))
            .unwrap();
        let health = fixture.health();
        assert_eq!(health.injector.status, "running");
        assert_eq!(health.injector.pid, Some(72001));
        let json = serde_json::to_value(health).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 3);
        for service in ["keymint", "keystore2", "injector"] {
            assert_eq!(json[service].as_object().unwrap().len(), 2);
        }

        std::fs::remove_dir_all(process).unwrap();
        assert_eq!(fixture.health().injector.status, "stopped");
    }

    #[test]
    fn same_named_process_and_stale_daemon_pid_are_not_injection() {
        let fixture = Fixture::new();
        let process =
            fixture.process(72001, &["/system/bin/keystore2"], Path::new("/system/bin/keystore2"));
        std::fs::write(process.join("maps"), "").unwrap();
        fixture.process(72002, &["/unrelated/inject"], Path::new("/unrelated/inject"));
        fixture.process(
            72003,
            &["/system/bin/sh", "/unrelated/daemon-injector"],
            Path::new("/system/bin/sh"),
        );
        std::fs::write(fixture.0.join("state/injector-daemon.pid"), "72003\n").unwrap();
        assert_eq!(fixture.health().injector.status, "stopped");
    }

    #[test]
    fn keymint_requires_deployed_inode_and_excludes_helper_commands() {
        let fixture = Fixture::new();
        let binary = fixture.0.join("state/keymint");
        let foreign = fixture.0.join("keymint");
        std::fs::write(&binary, "deployed").unwrap();
        std::fs::write(&foreign, "deployed").unwrap();
        fixture.process(72001, &["/foreign/keymint"], &foreign);
        fixture.process(72002, &[binary.to_str().unwrap(), "--webui-get-runtime-health"], &binary);
        assert_eq!(fixture.health().keymint.status, "stopped");
        fixture.process(72003, &[binary.to_str().unwrap()], &binary);
        assert_eq!(fixture.health().keymint.pid, Some(72003));
    }

    #[test]
    fn keymint_root_helper_and_startup_are_not_running() {
        let fixture = Fixture::new();
        let binary = fixture.0.join("state/keymint");
        std::fs::write(&binary, "deployed").unwrap();
        let helper = fixture.process(72001, &[binary.to_str().unwrap()], &binary);
        std::fs::write(helper.join("status"), "Uid:\t0\t0\t0\t0\n").unwrap();
        assert_eq!(fixture.health().keymint.status, "stopped");

        let startup = fixture.process(72002, &[binary.to_str().unwrap()], &binary);
        std::fs::write(startup.join("status"), "Uid:\t0\t0\t0\t0\n").unwrap();
        assert_eq!(fixture.health().keymint.status, "stopped");
        // Read the effective field rather than the real, saved or filesystem UID.
        std::fs::write(startup.join("status"), format!("Uid:\t0\t{KEYSTORE_UID}\t0\t0\n")).unwrap();
        assert_eq!(fixture.health().keymint.pid, Some(72002));
        std::fs::remove_file(startup.join("status")).unwrap();
        assert_eq!(fixture.health().keymint.status, "stopped");
    }

    #[test]
    fn deleted_keymint_requires_its_actual_inode_in_executable_mapping() {
        let fixture = Fixture::new();
        let binary = fixture.0.join("state/keymint");
        let deleted = fixture.0.join("state/keymint (deleted)");
        std::fs::write(&deleted, "old image").unwrap();
        std::fs::write(&binary, "new image").unwrap();
        let metadata = std::fs::metadata(&deleted).unwrap();
        let process = fixture.process(72001, &[binary.to_str().unwrap()], &deleted);
        std::fs::write(
            process.join("maps"),
            format!(
                "1000-2000 r-xp 0000 {:x}:{:x} {} {} (deleted)\n",
                libc::major(metadata.dev()),
                libc::minor(metadata.dev()),
                metadata.ino(),
                binary.display(),
            ),
        )
        .unwrap();
        assert_eq!(fixture.health().keymint.pid, Some(72001));
        std::fs::write(
            process.join("maps"),
            format!(
                "1000-2000 r-xp 0000 {:x}:{:x} {} {} (deleted)\n",
                libc::major(metadata.dev()),
                libc::minor(metadata.dev()),
                metadata.ino() + 1,
                binary.display(),
            ),
        )
        .unwrap();
        assert_eq!(fixture.health().keymint.status, "stopped");
    }

    #[test]
    fn payload_requires_current_pid_and_exact_random_mapping() {
        let maps = format!("1000-2000 r-xp 0000 00:01 42 /memfd:{IDENT} (deleted)\n");
        assert!(injected(&maps, &format!("123 {IDENT}\n"), 123, &[]));
        assert!(!injected(&maps, &format!("122 {IDENT}\n"), 123, &[]));
        assert!(!injected(&maps, &format!("123 {IDENT} extra\n"), 123, &[]));
        assert!(!injected(
            &maps.replace(IDENT, &format!("{IDENT}.other")),
            &format!("123 {IDENT}"),
            123,
            &[]
        ));
        assert!(!injected(&maps.replace("r-xp", "r--p"), &format!("123 {IDENT}"), 123, &[]));
        assert!(!injected(&maps, "123 lib.so", 123, &[]));
    }

    #[test]
    fn module_mapping_requires_exact_path_and_executable_image() {
        let binary = PathBuf::from("/data/adb/omk/inject");
        let maps = "1000-2000 r-xp 0000 00:01 42 /data/adb/omk/inject (deleted)\n";
        assert!(injected(maps, "", 123, std::slice::from_ref(&binary)));
        assert!(!injected(
            &maps.replace("inject (", "inject.other ("),
            "",
            123,
            std::slice::from_ref(&binary)
        ));
        assert!(!injected(&maps.replace("r-xp", "rw-p"), "", 123, &[binary]));
    }
}
