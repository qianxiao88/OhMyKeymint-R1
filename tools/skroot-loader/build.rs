use std::{env, fs, path::PathBuf};

fn value(key: &str, default: &str) -> String {
    println!("cargo:rerun-if-env-changed={key}");
    let value = env::var(key).unwrap_or_else(|_| default.into());
    assert!(!value.is_empty() && value.len() <= 200);
    assert!(value.bytes().all(|b| b.is_ascii_graphic() || b == b' '));
    value
}

fn main() {
    let id = value("OMK_SKROOT_ID32", "3e5017f4de9b4d86a25d7d320fbd29e2");
    assert!(id.len() == 32 && id.bytes().all(|b| b.is_ascii_alphanumeric()));
    let version = value("OMK_SKROOT_VERSION", "0.1.0-dev");
    let variant = value("OMK_SKROOT_VARIANT", "lite");
    assert!(["full", "no-soter", "lite"].contains(&variant.as_str()));
    let name = value("OMK_SKROOT_NAME", &format!("OhMyKeymint-R1 {variant}"));
    let author = value("OMK_SKROOT_AUTHOR", "OhMyKeymint-R1 contributors");
    let desc =
        value("OMK_SKROOT_DESC", "Native loader; effective SELinux policy preflight required");
    let fields = [
        (
            "NAME",
            "de1f663bcae792d7c9ed1a21cdaeff00331fe11b074f0303e018f43998f8db3e",
            name.as_str(),
            "04429c3f7172e4cce7ac7a5d44b75631ce91777b5f2a5eec076a3a508a76fb17",
        ),
        (
            "VERSION",
            "7dc74a2b86805f2cd0f45c6a82d384fd568e341c6f05af17c7229c9ba6d4318b",
            version.as_str(),
            "8dbc019a6e51fe8d4a3db9feaf05535d70879f35626050cd4131bfe323ecefc2",
        ),
        (
            "DESC",
            "05c62cd446c811e772c94d97fd546738328f7c6b9967ed135190ef37ff06d866",
            desc.as_str(),
            "ed02c95e091c66100d6f2f4554f96392b2951b394ba042278eed8f236f43e1a3",
        ),
        (
            "AUTHOR",
            "1f39b220291a2fb6e4fec95f179d0faf833ca534ef8ab2ffc4bd893768694451",
            author.as_str(),
            "d409c886fd6cedd694c1b126fd19eccd90e2463e04154c193effc7103ce63f1c",
        ),
        (
            "ID32",
            "b33dcddd88c07d079a8e0e9ec6ab976f15f2edba46cab91a61234cfbb982111c",
            id.as_str(),
            "3020d9a6c07a30e2649dd29a72d7e57121f0c0ca457b6d21dca5a69acbe6c2c4",
        ),
        (
            "MIN_SDK",
            "d2b0e8ccfab9a93f98e1e1bd2476f1ff58dd5ccb88b2228e93587e68dddd526c",
            "4.3.7",
            "162c41660d1b17650087d7bd2d946956407165b6008b970e8fe386f2de805f4b",
        ),
        ("INSTALL", "za5hrnp5r0rjm1dnjld7xoc6tza0qvdd", "", "v2ltlw5plpqec7yr5ifeuta24l3rv7ep"),
        ("UNINSTALL", "hft0lrndcqfoff3vankyj5drv30apccd", "", "p6tv4rinog5a0rl83tzp2ac6fl0uyqhk"),
    ];
    let mut code = format!("pub const ID32: &str = {id:?};\npub const VERSION: &str = {version:?};\npub const VARIANT: &str = {variant:?};\n");
    for (key, begin, val, end) in fields {
        let bytes = format!("{begin}{val}{end}\0").into_bytes();
        code.push_str(&format!(
            "#[used]\n#[no_mangle]\npub static OMK_SKROOT_META_{key}: [u8; {}] = {bytes:?};\n",
            bytes.len()
        ));
    }
    fs::write(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("metadata.rs"), code).unwrap();
}
