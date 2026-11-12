use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const MODULE_ROOT: &str = "/data/adb/modules/oh_my_keymint";
pub const STATE_ROOT: &str = "/data/misc/keystore/omk/data/skroot-loader";
pub const OWNER: &str = ".omk-skroot-owner.json";
const LIMIT: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub id32: String,
    pub version: String,
    pub variant: String,
    pub files: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub mode: u32,
    pub sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    schema: u32,
    id32: String,
    version: String,
    variant: String,
}

pub fn fail(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
    io::Error::other(message).into()
}

pub fn plain_path(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for part in path.components() {
        if !matches!(part, Component::RootDir | Component::Normal(_)) {
            return Err(fail("path has non-normal component"));
        }
        current.push(part);
        let meta = fs::symlink_metadata(&current)?;
        if meta.file_type().is_symlink() {
            return Err(fail("symlinked path"));
        }
    }
    Ok(())
}

pub fn create_plain_directory(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for part in path.components() {
        if !matches!(part, Component::RootDir | Component::Normal(_)) {
            return Err(fail("invalid directory component"));
        }
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => return Err(fail("non-directory ancestor")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub fn regular(path: &Path) -> Result<File> {
    plain_path(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() > LIMIT {
        return Err(fail("non-regular, linked or oversized package file"));
    }
    Ok(file)
}

pub fn relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && path.bytes().all(|b| b.is_ascii_alphanumeric() || b"/-_.+".contains(&b))
        && !path.starts_with('/')
        && path.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
        && !path.starts_with(".omk-skroot-")
        && !["disable", "remove", "update"].contains(&path)
}

pub fn read_manifest(private: &Path) -> Result<Manifest> {
    let file = regular(&private.join("omk-payload.json"))?;
    if file.metadata()?.len() > 2 * 1024 * 1024 {
        return Err(fail("manifest size limit"));
    }
    let manifest: Manifest = serde_json::from_reader(file)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.schema != 1
        || manifest.id32 != crate::ID32
        || manifest.version != crate::VERSION
        || manifest.variant != crate::VARIANT
        || manifest.files.len() > 8192
    {
        return Err(fail("manifest build identity mismatch"));
    }
    let mut paths = BTreeSet::new();
    for file in &manifest.files {
        if !relative(&file.path)
            || ![0o600, 0o644, 0o755].contains(&file.mode)
            || file.sha256.len() != 64
            || !file.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || !paths.insert(file.path.as_str())
        {
            return Err(fail("invalid manifest entry"));
        }
    }
    for required in [
        "module.prop",
        "post-fs-data.sh",
        "service.sh",
        "daemon",
        "daemon-injector",
        "sepolicy.rule",
        "arm64-v8a/r1-helper",
        "libs/arm64-v8a/keymint",
        "libs/arm64-v8a/inject",
    ] {
        if !paths.contains(required) {
            return Err(fail("required module resource missing"));
        }
    }
    for entry in &manifest.files {
        if [
            "post-fs-data.sh",
            "service.sh",
            "daemon",
            "daemon-injector",
            "arm64-v8a/r1-helper",
            "libs/arm64-v8a/keymint",
            "libs/arm64-v8a/inject",
            "bin/resetprop",
        ]
        .contains(&entry.path.as_str())
            && entry.mode != 0o755
        {
            return Err(fail("required executable mode"));
        }
    }
    Ok(())
}

pub fn verify_payload(private: &Path, manifest: &Manifest) -> Result<()> {
    let payload = private.join("payload");
    plain_path(&payload)?;
    let wanted: BTreeSet<_> = manifest.files.iter().map(|f| f.path.clone()).collect();
    let mut actual = BTreeSet::new();
    scan(&payload, &payload, &mut actual)?;
    if actual != wanted {
        return Err(fail("payload file set mismatch"));
    }
    for entry in &manifest.files {
        if hash_reader(regular(&payload.join(&entry.path))?)? != entry.sha256.to_ascii_lowercase() {
            return Err(fail("payload digest mismatch"));
        }
    }
    let prop = fs::read_to_string(payload.join("module.prop"))?;
    if !prop.lines().any(|l| l == "id=oh_my_keymint") {
        return Err(fail("module id mismatch"));
    }
    Ok(())
}

fn scan(base: &Path, dir: &Path, output: &mut BTreeSet<String>) -> Result<()> {
    if output.len() > 8192 {
        return Err(fail("payload file limit"));
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let rel =
            path.strip_prefix(base)?.to_str().ok_or_else(|| fail("non-UTF8 filename"))?.to_owned();
        if !relative(&rel) {
            return Err(fail("invalid payload path"));
        }
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            scan(base, &path, output)?;
        } else if meta.is_file() {
            output.insert(rel);
        } else {
            return Err(fail("payload contains special file"));
        }
    }
    Ok(())
}

fn hash_reader(mut file: File) -> Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn owned(root: &Path) -> Result<bool> {
    if !root.try_exists()? {
        return Ok(false);
    }
    plain_path(root)?;
    let owner: Ownership = serde_json::from_reader(regular(&root.join(OWNER))?)?;
    if owner.schema != 1 || owner.id32 != crate::ID32 {
        return Err(fail("foreign module owner"));
    }
    Ok(true)
}

pub fn disabled(root: &Path) -> Result<bool> {
    for name in ["disable", "remove"] {
        if root.join(name).try_exists()? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn install(private: &Path, root: &Path, manifest: &Manifest) -> Result<()> {
    let exists = owned(root)?;
    if exists && disabled(root)? {
        return Err(fail("module is disabled or pending removal"));
    }
    let parent = root.parent().ok_or_else(|| fail("module parent missing"))?;
    plain_path(parent)?;
    let stage = parent.join(format!(".oh_my_keymint.skroot-stage-{}", std::process::id()));
    fs::create_dir(&stage)?;
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))?;
    let result = (|| {
        for entry in &manifest.files {
            let dest = stage.join(&entry.path);
            let dir = dest.parent().ok_or_else(|| fail("invalid resource parent"))?;
            fs::create_dir_all(dir)?;
            let mut source = regular(&private.join("payload").join(&entry.path))?;
            let mut out =
                OpenOptions::new().write(true).create_new(true).mode(0o600).open(&dest)?;
            let copied = io::copy(&mut Read::by_ref(&mut source).take(LIMIT + 1), &mut out)?;
            if copied > LIMIT {
                return Err(fail("resource grew beyond size limit"));
            }
            out.flush()?;
            out.set_permissions(fs::Permissions::from_mode(entry.mode))?;
            out.sync_all()?;
            if hash_reader(regular(&dest)?)? != entry.sha256.to_ascii_lowercase() {
                return Err(fail("staged resource digest mismatch"));
            }
        }
        fix_directories(&stage)?;
        let owner = Ownership {
            schema: 1,
            id32: crate::ID32.into(),
            version: manifest.version.clone(),
            variant: manifest.variant.clone(),
        };
        let mut out =
            OpenOptions::new().write(true).create_new(true).mode(0o600).open(stage.join(OWNER))?;
        serde_json::to_writer(&mut out, &owner)?;
        out.sync_all()?;
        File::open(&stage)?.sync_all()?;
        // Recheck immediately before the atomic operation, never remove a foreign tree.
        if owned(root)? != exists {
            return Err(fail("module ownership changed"));
        }
        if exists && disabled(root)? {
            return Err(fail("module disabled during staging"));
        }
        atomic_rename(
            &stage,
            root,
            if exists { 2 } else { 1 }, // Linux renameat2 EXCHANGE / NOREPLACE
        )?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if stage.try_exists()? {
        fs::remove_dir_all(&stage)?;
    }
    result
}

fn fix_directories(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if fs::symlink_metadata(&path)?.is_dir() {
            fix_directories(&path)?;
        }
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

pub fn atomic_rename(from: &Path, to: &Path, flags: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            flags,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

pub fn remove_owned(root: &Path) -> Result<()> {
    if !owned(root)? {
        return Ok(());
    }
    let removed =
        root.with_file_name(format!(".oh_my_keymint.skroot-remove-{}", std::process::id()));
    atomic_rename(root, &removed, 1)?; // Linux RENAME_NOREPLACE
    if owned(&removed)? {
        fs::remove_dir_all(removed)?;
    }
    Ok(())
}
