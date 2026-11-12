use super::*;
use crate::package::{Entry, Manifest};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "omk-skroot-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn payload(&self) -> (PathBuf, Manifest) {
        let private = self.0.join("private");
        fs::create_dir_all(private.join("payload/libs/arm64-v8a")).unwrap();
        fs::create_dir_all(private.join("payload/arm64-v8a")).unwrap();
        let mut files = Vec::new();
        for (path, mode, bytes) in [
            ("module.prop", 0o644, "id=oh_my_keymint\nversion=test\n"),
            ("post-fs-data.sh", 0o755, "exit 0\n"),
            ("service.sh", 0o755, "exit 0\n"),
            ("daemon", 0o755, "daemon\n"),
            ("daemon-injector", 0o755, "injector\n"),
            ("sepolicy.rule", 0o644, "allow keystore system_file file { map read }\n"),
            ("arm64-v8a/r1-helper", 0o755, "helper\n"),
            ("libs/arm64-v8a/keymint", 0o755, "keymint\n"),
            ("libs/arm64-v8a/inject", 0o755, "inject\n"),
        ] {
            fs::write(private.join("payload").join(path), bytes).unwrap();
            files.push(Entry {
                path: path.into(),
                mode,
                sha256: format!("{:x}", Sha256::digest(bytes.as_bytes())),
            });
        }
        let manifest = Manifest {
            schema: 1,
            id32: crate::ID32.into(),
            version: crate::VERSION.into(),
            variant: crate::VARIANT.into(),
            files,
        };
        fs::write(private.join("omk-payload.json"), serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        (private, manifest)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn manifest_rejects_traversal_duplicates_and_wrong_identity() {
    let fixture = Fixture::new();
    let (_, mut manifest) = fixture.payload();
    package::validate_manifest(&manifest).unwrap();
    let file = manifest.files[0].clone();
    manifest.files.push(file);
    assert!(package::validate_manifest(&manifest).is_err());
    manifest.files.pop();
    manifest.files[0].path = "../module.prop".into();
    assert!(package::validate_manifest(&manifest).is_err());
    assert!(!package::relative("/absolute"));
    assert!(!package::relative("a//b"));
    assert!(!package::relative("disable"));
    assert!(!package::relative("a/./b"));
}

#[test]
fn payload_requires_exact_files_hashes_and_no_symlinks() {
    let fixture = Fixture::new();
    let (private, manifest) = fixture.payload();
    package::verify_payload(&private, &manifest).unwrap();
    fs::write(private.join("payload/service.sh"), "changed").unwrap();
    assert!(package::verify_payload(&private, &manifest).is_err());
    fs::remove_file(private.join("payload/service.sh")).unwrap();
    std::os::unix::fs::symlink("/dev/null", private.join("payload/service.sh")).unwrap();
    assert!(package::verify_payload(&private, &manifest).is_err());
}

#[test]
fn ownership_guards_update_disable_and_uninstall() {
    let fixture = Fixture::new();
    let (private, manifest) = fixture.payload();
    let root = fixture.0.join("oh_my_keymint");
    package::install(&private, &root, &manifest).unwrap();
    assert!(package::owned(&root).unwrap());
    assert_eq!(fs::metadata(root.join("daemon")).unwrap().permissions().mode() & 0o777, 0o755);
    package::install(&private, &root, &manifest).unwrap();
    fs::write(root.join("disable"), "").unwrap();
    assert!(package::install(&private, &root, &manifest).is_err());
    package::remove_owned(&root).unwrap();
    assert!(!root.exists());
    fs::create_dir(&root).unwrap();
    fs::write(root.join("foreign"), "manager").unwrap();
    assert!(package::install(&private, &root, &manifest).is_err());
    assert!(package::remove_owned(&root).is_err());
    assert!(root.join("foreign").exists());
}

#[test]
fn duplicate_lifetime_lock_is_exclusive_and_survives_file_lifetime() {
    let fixture = Fixture::new();
    let path = fixture.0.join("lock");
    let lock = Lock::acquire(&path, Duration::ZERO).unwrap();
    assert!(Lock::acquire(&path, Duration::ZERO).is_err());
    drop(lock);
    Lock::acquire(&path, Duration::ZERO).unwrap();
}

#[test]
fn policy_parser_expands_sets_and_blocks_unparsed_syntax() {
    let rules = policy::parse_rules(
        "# comment\nallow { keystore system_app } system_file file { map read }\n",
    )
    .unwrap();
    assert_eq!(rules.len(), 4);
    assert!(policy::parse_rules("permissive keystore").is_err());
    assert!(policy::parse_rules("allow keystore system_file file { map").is_err());
    assert!(policy::parse_rules("allow keystore system_file file map garbage").is_err());
}

#[test]
fn metadata_has_upstream_markers_and_exact_identity() {
    assert_eq!(crate::ID32.len(), 32);
    assert!(crate::OMK_SKROOT_META_NAME
        .starts_with(b"de1f663bcae792d7c9ed1a21cdaeff00331fe11b074f0303e018f43998f8db3e"));
    assert!(crate::OMK_SKROOT_META_MIN_SDK.windows(5).any(|s| s == b"4.3.7"));
}
