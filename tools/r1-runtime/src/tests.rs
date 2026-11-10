use super::*;
use serde_json::json;

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "r1-test-{}-{}-{sequence}",
            std::process::id(),
            now()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn paths(&self) -> Paths {
        let paths = Paths { module: self.0.join("module"), data: self.0.join("data") };
        fs::create_dir_all(&paths.module).unwrap();
        paths.prepare().unwrap();
        paths
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn build() -> BuildInfo {
    BuildInfo::parse(br#"{"version":"1.3.8-301-full","hash":"0123456789abcdef","variant":"full"}"#)
        .unwrap()
}

fn observation(connected: bool) -> CloudObservation {
    CloudObservation { checked_at: now(), connected, reason: "cloud stats invalid devices".into() }
}

fn diagnostics() -> Value {
    json!({
        "keymint": {"status": "running", "pid": 123},
        "keystore2": {"status": "running", "pid": 124},
        "injector": {"status": "running", "pid": 125},
        "tee": {"status": "available"}
    })
}

fn stats() -> Value {
    json!({
        "devices": 0, "peak": 12, "keybox_distinct": 0,
        "product": "OhMyKeymint-R1", "version": "1.3.8-301-full",
        "version_code": 301, "updated_at": "2026-11-10 12:00:00"
    })
}

#[test]
fn diagnostics_requires_every_running_service_and_positive_pid() {
    assert!(validate_diagnostics(&serde_json::to_vec(&diagnostics()).unwrap()).is_ok());
    for name in ["keymint", "keystore2", "injector"] {
        let mut missing = diagnostics();
        missing.as_object_mut().unwrap().remove(name);
        assert!(validate_diagnostics(&serde_json::to_vec(&missing).unwrap())
            .unwrap_err()
            .contains(name));
        for state in ["stopped", "configured", "disabled", "unknown", "available", "error", ""] {
            let mut value = diagnostics();
            value[name]["status"] = json!(state);
            assert!(validate_diagnostics(&serde_json::to_vec(&value).unwrap()).is_err());
        }
        for pid in [json!(null), json!(0), json!(-1), json!(1.5), json!("123"), json!(u64::MAX)] {
            let mut value = diagnostics();
            value[name]["pid"] = pid;
            assert!(validate_diagnostics(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    for bytes in [b"null".as_slice(), b"[]", b"true", b"not json"] {
        assert!(validate_diagnostics(bytes).is_err());
    }
}

#[test]
fn stats_requires_protocol_fields_and_correct_types() {
    assert!(validate_stats(&serde_json::to_vec(&stats()).unwrap()).is_ok());
    for field in
        ["devices", "peak", "keybox_distinct", "version_code", "product", "version", "updated_at"]
    {
        let mut missing = stats();
        missing.as_object_mut().unwrap().remove(field);
        assert!(validate_stats(&serde_json::to_vec(&missing).unwrap()).is_err());
        for wrong in [json!(null), json!(true), json!([]), json!({})] {
            let mut value = stats();
            value[field] = wrong;
            assert!(validate_stats(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    for field in ["devices", "peak", "keybox_distinct", "version_code"] {
        for wrong in [json!(-1), json!(0.5), json!("1")] {
            let mut value = stats();
            value[field] = wrong;
            assert!(validate_stats(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    let mut wrong_product = stats();
    wrong_product["product"] = json!("other");
    assert!(validate_stats(&serde_json::to_vec(&wrong_product).unwrap()).is_err());
    for bytes in [b"null".as_slice(), b"[]", b"{\"ok\":true}", b"<html>error</html>"] {
        assert!(validate_stats(bytes).is_err());
    }
}

#[test]
fn response_limits_include_exact_boundary_and_chunked_reader() {
    for limit in [STATS_LIMIT, KEYBOX_LIMIT] {
        assert_eq!(read_limited(&vec![b'x'; limit][..], limit).unwrap().len(), limit);
        assert!(read_limited(&vec![b'x'; limit + 1][..], limit).is_err());
        assert!(read_limited(std::io::repeat(b'x'), limit).is_err());
    }
    assert!(validate_stats(&vec![b' '; STATS_LIMIT + 1]).is_err());
    assert!(read_limited(std::io::empty(), 0).unwrap().is_empty());
}

#[test]
fn keybox_comparison_is_bounded_read_only_and_byte_exact() {
    let root = TestRoot::new();
    let path = root.0.join("keybox.xml");
    assert!(!keybox_unchanged(&path, b"abc"));
    assert!(!path.exists());
    fs::write(&path, b"abc").unwrap();
    assert!(keybox_unchanged(&path, b"abc"));
    assert!(!keybox_unchanged(&path, b"abd"));
    assert!(!keybox_unchanged(&path, b"abc\n"));
    assert!(read_file(&path, KEYBOX_LIMIT).unwrap() == b"abc");
    let maximum = vec![b'x'; KEYBOX_LIMIT];
    fs::write(&path, &maximum).unwrap();
    assert!(keybox_unchanged(&path, &maximum));
    let oversized = vec![b'x'; KEYBOX_LIMIT + 1];
    fs::write(&path, &oversized).unwrap();
    assert!(!keybox_unchanged(&path, &oversized));
    assert!(!keybox_unchanged(&root.0, b"abc"));
}

#[test]
fn keybox_hash_is_required_strict_and_case_insensitive() {
    let bytes = b"abc";
    let hash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert!(verify_keybox(bytes, hash).is_ok());
    assert!(verify_keybox(bytes, &hash.to_uppercase()).is_ok());
    for wrong in ["".to_string(), "0".repeat(63), "0".repeat(65), "z".repeat(64), "0".repeat(64)] {
        assert!(verify_keybox(bytes, &wrong).is_err());
    }
    assert!(verify_keybox(b"", hash).is_err());
    assert!(verify_keybox(&vec![0; KEYBOX_LIMIT + 1], hash).is_err());
    let maximum = vec![b'x'; KEYBOX_LIMIT];
    let digest = format!("{:x}", Sha256::digest(&maximum));
    assert!(verify_keybox(&maximum, &digest).is_ok());
}

#[test]
fn build_metadata_rejects_bad_hash_version_or_variant() {
    assert_eq!(build().version, "1.3.8-301-full");
    assert!(BuildInfo::parse(
        br#"{"version":"1.3.8-301-no-soter","hash":"abc1234","variant":"no-soter"}"#
    )
    .is_ok());
    for hash in ["", "abc123", "g123456", "abc1234\n", "../../../x"] {
        let value = json!({"version":"1.0", "hash":hash, "variant":"full"});
        assert!(BuildInfo::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for hash in ["a".repeat(7), "A".repeat(40), "f".repeat(64)] {
        let value = json!({"version":"1.0", "hash":hash, "variant":"lite"});
        assert!(BuildInfo::parse(&serde_json::to_vec(&value).unwrap()).is_ok());
    }
    for (version, hash, variant) in
        [("1\nname=bad", "abc1234", "full"), ("", "abc1234", "full"), ("1.0", "abc1234", "unknown")]
    {
        let value = json!({"version":version, "hash":hash, "variant":variant});
        assert!(BuildInfo::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}

#[test]
fn success_requires_local_services_and_fresh_verified_cloud() {
    let cloud = observation(true);
    let good = status(Ok(()), &cloud);
    assert!(good.ok && good.cloud_connected);
    let text = description(&build(), DisplayState::Checked(&good));
    assert_eq!(text, concat!(
        "\u{2705} OMK-R1-\u{ff08}1.3.8-301-full\u{ff09}-\u{ff08}9abcdef\u{ff09}",
        "\u{5df2}\u{6210}\u{529f}\u{8fd0}\u{884c}\u{3002}\u{5df2}\u{6210}\u{529f}\u{8fde}\u{63a5}\u{4e91}\u{7aef}\u{3002}",
        "\u{7b49}\u{7740}\u{88ab}\u{6211}\u{683c}\u{4f60}\u{673a}\u{673a}\u{5427}\u{1f604} ",
        "\u{3010}\u{672c}\u{6a21}\u{5757}\u{7531}\u{3010}\u{6d45}\u{7b11}\u{5450}\u{3011}\u{4e8c}\u{6539}\u{81ea}\u{7528}\u{ff0c}\u{5c5e}\u{514d}\u{8d39}\u{6a21}\u{5757}\u{ff0c}\u{5012}\u{5356}\u{53ef}\u{803b}\u{3002}\u{3011}"
    ));
    assert!(!status(Err("injector is stopped".into()), &cloud).ok);
    assert!(!status(Err(String::new()), &cloud).ok);
    assert!(!status(Ok(()), &observation(false)).ok);
    let expired =
        CloudObservation { checked_at: now() - CLOUD_INTERVAL.as_secs(), ..observation(true) };
    assert!(!status(Ok(()), &expired).cloud_connected);
    let future = CloudObservation { checked_at: now() + 1000, ..observation(true) };
    assert!(!status(Ok(()), &future).ok);
    let contradictory =
        Status { ok: true, reason: "cloud unavailable".into(), cloud_connected: false };
    assert!(description(&build(), DisplayState::Checked(&contradictory)).starts_with('\u{274c}'));
}

#[test]
fn failure_reason_is_specific_and_single_line() {
    let checked = status(
        Err("injector stopped\r\nname=spoof\0\u{2028}\u{2029}test".into()),
        &observation(true),
    );
    let text = description(&build(), DisplayState::Checked(&checked));
    assert!(text.contains("injector stopped name=spoof test"));
    assert!(text.starts_with("\u{274c} OMK-R1 \u{8fd0}\u{884c}\u{5931}\u{8d25}\u{3002}\u{ff08}"));
    assert!(text.ends_with("\u{ff09}\u{4f60}\u{5378}\u{8f7d}\u{5427}\u{3002}"));
    assert!(!text.chars().any(char::is_control));
    assert_eq!(single_line(&"a".repeat(500)).len(), 240);
    let checked = status(Err("a".repeat(500)), &observation(true));
    let prop = replace_description(
        "description=old\n",
        &description(&build(), DisplayState::Checked(&checked)),
    );
    assert!(prop.ends_with("\u{ff09}\u{4f60}\u{5378}\u{8f7d}\u{5427}\u{3002}\n"));
}

#[test]
fn waiting_is_distinct_and_grace_expires_at_120_seconds() {
    let failed = Err("keymint is stopped".into());
    assert!(startup_wait(&failed, Duration::ZERO));
    assert!(startup_wait(&failed, Duration::from_secs(119)));
    assert!(!startup_wait(&failed, Duration::from_secs(120)));
    assert!(!startup_wait(&Ok(()), Duration::ZERO));
    let text = description(&build(), DisplayState::Waiting("waiting\nfor boot"));
    assert!(text.starts_with("OMK-R1 "));
    assert!(!text.contains('\u{2705}') && !text.contains('\u{274c}'));
    assert!(text.ends_with("waiting for boot"));
}

#[test]
fn module_prop_preserves_other_fields_author_and_line_endings() {
    let original = "id=omk\r\nname=R1\nauthor=packager\nversion=1\ndescription=old\ncustom=yes";
    let changed = replace_description(original, "checked\nname=bad");
    assert_eq!(
        changed,
        "id=omk\r\nname=R1\nauthor=packager\nversion=1\ndescription=checked name=bad\ncustom=yes"
    );
    assert_eq!(replace_description("id=omk", "new"), "id=omk\ndescription=new\n");
    assert_eq!(replace_description("", "new"), "description=new\n");
    assert_eq!(
        replace_description("description=a\n description=b\nauthor=c\n", "new"),
        "description=new\nauthor=c\n"
    );
}

#[test]
fn atomic_description_update_preserves_mode_and_is_idempotent() {
    let root = TestRoot::new();
    let paths = root.paths();
    let prop = paths.module.join("module.prop");
    fs::write(&prop, "id=omk\nauthor=packager\ndescription=old\n").unwrap();
    fs::set_permissions(&prop, fs::Permissions::from_mode(0o640)).unwrap();
    update_description(&paths, &build(), DisplayState::Waiting("boot")).unwrap();
    let first = fs::read(&prop).unwrap();
    assert_eq!(fs::metadata(&prop).unwrap().permissions().mode() & 0o7777, 0o640);
    update_description(&paths, &build(), DisplayState::Waiting("boot")).unwrap();
    assert_eq!(fs::read(&prop).unwrap(), first);
    assert!(String::from_utf8(first).unwrap().contains("author=packager\n"));
    assert_eq!(fs::read_dir(&paths.module).unwrap().count(), 1);
}

#[test]
fn symlinked_module_prop_is_rejected() {
    let root = TestRoot::new();
    let paths = root.paths();
    let target = paths.module.join("real.prop");
    fs::write(&target, "author=packager\n").unwrap();
    std::os::unix::fs::symlink(&target, paths.module.join("module.prop")).unwrap();
    assert!(update_description(&paths, &build(), DisplayState::Waiting("boot")).is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "author=packager\n");
}

#[test]
fn temporary_keybox_is_private_and_removed_on_error_unwind() {
    let root = TestRoot::new();
    let paths = root.paths();
    let mut temporary = PathBuf::new();
    let result: Result<()> = (|| {
        let temp = TempFile::create(&paths.data, "keybox")?;
        temporary = temp.path.clone();
        assert_eq!(temp.file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        verify_keybox(b"abc", "missing")
    })();
    assert!(result.is_err());
    assert!(!temporary.exists());
}

#[test]
fn file_lock_is_exclusive_released_and_not_unlinked() {
    let root = TestRoot::new();
    let path = root.0.join("monitor.lock");
    let first = Lock::acquire(&path, Duration::ZERO).unwrap();
    assert!(Lock::acquire(&path, Duration::ZERO).is_err());
    drop(first);
    assert!(path.exists());
    assert!(Lock::acquire(&path, Duration::ZERO).is_ok());
}

#[test]
fn cloud_cache_is_private_and_stale_or_corrupt_data_fails_closed() {
    let root = TestRoot::new();
    let paths = root.paths();
    assert!(!CloudObservation::load(&paths).connected);
    observation(true).save(&paths).unwrap();
    assert!(CloudObservation::load(&paths).connected);
    let path = paths.data.join("cloud.json");
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let expired = CloudObservation { checked_at: 0, ..observation(true) };
    expired.save(&paths).unwrap();
    assert!(!CloudObservation::load(&paths).connected);
    fs::write(&path, b"bad json").unwrap();
    assert!(!CloudObservation::load(&paths).connected);
}

#[test]
fn native_command_timeout_is_bounded() {
    let sleep = if cfg!(target_os = "android") { "/system/bin/sleep" } else { "/bin/sleep" };
    let started = Instant::now();
    let error = command_output(Path::new(sleep), &["10".into()], Duration::from_millis(100), 128)
        .unwrap_err();
    assert!(error.contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn native_command_output_is_limited_and_spawn_failure_is_safe() {
    let echo = if cfg!(target_os = "android") { "/system/bin/echo" } else { "/bin/echo" };
    assert_eq!(
        command_output(Path::new(echo), &["ok".into()], Duration::from_secs(2), 3).unwrap(),
        b"ok\n"
    );
    assert!(command_output(Path::new(echo), &["long output".into()], Duration::from_secs(2), 3)
        .is_err());
    assert!(command_output(
        Path::new("/nonexistent/r1-test-program"),
        &[],
        Duration::from_secs(1),
        128
    )
    .is_err());
}
