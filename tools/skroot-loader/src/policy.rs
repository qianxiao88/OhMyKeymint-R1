//! Read-only effective-policy queries. This module never loads or patches policy.
use crate::package::{fail, Result};
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    fs,
    path::Path,
};

#[repr(C)]
#[derive(Default)]
struct Decision {
    allowed: u32,
    decided: u32,
    auditallow: u32,
    auditdeny: u32,
    seqno: u32,
    flags: u32,
}
type Class = unsafe extern "C" fn(*const c_char) -> u16;
type Perm = unsafe extern "C" fn(u16, *const c_char) -> u32;
type Compute = unsafe extern "C" fn(*const c_char, *const c_char, u16, u32, *mut Decision) -> c_int;
type Getcon = unsafe extern "C" fn(*mut *mut c_char) -> c_int;
type Freecon = unsafe extern "C" fn(*mut c_char);

struct Api {
    handle: *mut c_void,
    class: Class,
    perm: Perm,
    compute: Compute,
    getcon: Getcon,
    freecon: Freecon,
}
impl Drop for Api {
    fn drop(&mut self) {
        unsafe {
            libc::dlclose(self.handle);
        }
    }
}
impl Api {
    fn open() -> Result<Self> {
        let handle = unsafe {
            libc::dlopen(c"/system/lib64/libselinux.so".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL)
        };
        if handle.is_null() {
            return Err(fail("SELinux query library unavailable"));
        }
        macro_rules! symbol {
            ($name:literal, $ty:ty) => {{
                let name = CString::new($name)?;
                let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
                if ptr.is_null() {
                    unsafe {
                        libc::dlclose(handle);
                    }
                    return Err(fail("SELinux query symbol unavailable"));
                }
                unsafe { std::mem::transmute::<*mut c_void, $ty>(ptr) }
            }};
        }
        Ok(Self {
            handle,
            class: symbol!("string_to_security_class", Class),
            perm: symbol!("string_to_av_perm", Perm),
            compute: symbol!("security_compute_av_flags_raw", Compute),
            getcon: symbol!("getcon_raw", Getcon),
            freecon: symbol!("freecon", Freecon),
        })
    }
    fn current(&self) -> Result<String> {
        let mut ptr = std::ptr::null_mut();
        if unsafe { (self.getcon)(&mut ptr) } < 0 || ptr.is_null() {
            return Err(fail("SELinux current context unavailable"));
        }
        let result = unsafe { CStr::from_ptr(ptr) }.to_str().map(str::to_owned);
        unsafe { (self.freecon)(ptr) };
        Ok(result?)
    }
    fn check(&self, source: &str, target: &str, class: &str, perm: &str) -> Result<()> {
        let (source, target, class_name, perm_name) = (
            CString::new(source)?,
            CString::new(target)?,
            CString::new(class)?,
            CString::new(perm)?,
        );
        let class_id = unsafe { (self.class)(class_name.as_ptr()) };
        let mask = unsafe { (self.perm)(class_id, perm_name.as_ptr()) };
        if class_id == 0 || mask == 0 {
            return Err(fail("SELinux class or permission missing"));
        }
        let mut decision = Decision::default();
        let rc = unsafe {
            (self.compute)(source.as_ptr(), target.as_ptr(), class_id, mask, &mut decision)
        };
        if rc < 0 || decision.allowed & mask != mask || decision.flags & 1 != 0 {
            return Err(fail(&format!(
                "effective SELinux policy missing: {} {} {class} {perm}",
                source.to_string_lossy(),
                target.to_string_lossy()
            )));
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Rule {
    pub source: String,
    pub target: String,
    pub class: String,
    pub permission: String,
}

fn token_set(tokens: &[&str], cursor: &mut usize) -> Result<Vec<String>> {
    let token = *tokens.get(*cursor).ok_or_else(|| fail("truncated SELinux rule"))?;
    *cursor += 1;
    if token != "{" {
        return Ok(vec![token.into()]);
    }
    let mut values = Vec::new();
    while let Some(token) = tokens.get(*cursor) {
        *cursor += 1;
        if *token == "}" {
            if values.is_empty() {
                break;
            }
            return Ok(values);
        }
        values.push((*token).into());
    }
    Err(fail("invalid SELinux set"))
}

pub fn parse_rules(text: &str) -> Result<Vec<Rule>> {
    let mut rules = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let tokens: Vec<_> = line.split_whitespace().collect();
        if tokens.first() != Some(&"allow") {
            return Err(fail("unsupported SELinux rule syntax"));
        }
        let mut at = 1;
        let sources = token_set(&tokens, &mut at)?;
        let targets = token_set(&tokens, &mut at)?;
        let classes = token_set(&tokens, &mut at)?;
        let perms = token_set(&tokens, &mut at)?;
        if at != tokens.len() {
            return Err(fail("trailing SELinux tokens"));
        }
        for source in &sources {
            for target in &targets {
                for class in &classes {
                    for permission in &perms {
                        for value in [source, target, class, permission] {
                            if !value
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'*')
                            {
                                return Err(fail("invalid SELinux token"));
                            }
                        }
                        rules.push(Rule {
                            source: source.clone(),
                            target: target.clone(),
                            class: class.clone(),
                            permission: permission.clone(),
                        });
                    }
                }
            }
        }
    }
    if rules.is_empty() {
        return Err(fail("empty SELinux requirements"));
    }
    Ok(rules)
}

fn context(kind: &str) -> String {
    format!(
        "u:{}:{kind}:s0",
        if ["system_file", "default_android_service"].contains(&kind) { "object_r" } else { "r" }
    )
}

pub fn preflight(payload: &Path) -> Result<()> {
    if fs::read_to_string("/sys/fs/selinux/enforce")?.trim() != "1" {
        return Err(fail("SELinux enforcing is required; loader never changes it"));
    }
    let api = Api::open()?;
    let current = api.current()?;
    // The daemon keeps this SID while changing UID/GID. Verify its actual SID,
    // not an assumption that uid 0 or setuid(1017) confers a keystore domain.
    api.check(&current, &current, "process", "setsockcreate")?;
    for permission in ["setuid", "setgid"] {
        api.check(&current, &current, "capability", permission)?;
    }
    for permission in ["read", "open", "execute", "execute_no_trans", "map"] {
        api.check(&current, "u:object_r:system_file:s0", "file", permission)?;
    }
    for permission in ["create", "bind", "read", "write"] {
        api.check(&current, "u:object_r:system_file:s0", "unix_dgram_socket", permission)?;
    }
    let rules = parse_rules(&fs::read_to_string(payload.join("sepolicy.rule"))?)?;
    for rule in rules {
        let perms = if rule.permission == "*" {
            fs::read_dir(format!("/sys/fs/selinux/class/{}/perms", rule.class))?
                .map(|entry| {
                    entry?.file_name().into_string().map_err(|_| fail("invalid SELinux permission"))
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            vec![rule.permission]
        };
        for perm in perms {
            api.check(&context(&rule.source), &context(&rule.target), &rule.class, &perm)?;
        }
    }
    Ok(())
}

pub fn label_executable(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes())?;
    let label = c"u:object_r:system_file:s0";
    let rc = unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            c"security.selinux".as_ptr(),
            label.as_ptr().cast(),
            label.to_bytes_with_nul().len(),
            0,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}
