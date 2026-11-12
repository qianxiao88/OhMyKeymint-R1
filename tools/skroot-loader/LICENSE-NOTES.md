# Native loader licensing and provenance

The new OMK loader source follows the repository's [General License Agreement](../../LICENSE.md).
Existing OMK payloads retain their own notices and licenses. This file does not
relicense upstream OMK components or bundled third-party programs.

This implementation uses Rust/libc ABI declarations and independently written
resource/lifecycle code. It does not link, embed, copy or ship SKRoot's proprietary
`libkernel_module_kit_static.a`, SDK implementation, C++ runtime, or example source.
The exact exported entry names and descriptor marker bytes implement the public
SKRootPro binary interoperation contract. Upstream headers, examples, the official
development DOCX and read-only archive metadata were consulted at:

https://github.com/abcz316/SKRoot-linuxKernelRoot/tree/master/Pro%28%E4%BC%97%E6%B5%8B%E5%BC%80%E6%94%BE%E4%B8%AD%29/src/testModule

The inspected kit archive's minimum supported SDK marker is `4.3.7`; the loader
retains this marker as a conservative parser requirement. It calls no SDK kernel
or root_key API.

Direct crates: libc (MIT OR Apache-2.0), serde and serde_json (MIT OR Apache-2.0),
sha2 (MIT OR Apache-2.0). Their transitive license texts remain applicable; the
packager should retain crate notices alongside the payload's existing notices.
Android's platform libselinux is dynamically opened at runtime, never redistributed
by this crate. No resetprop, SELinux policy loader or Zygisk provider is vendored.
If a packager adds one, that component requires its own source/provenance and
license notice in the package.
