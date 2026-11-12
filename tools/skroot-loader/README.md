# SKRootPro native loader

Independent Rust workspace for Android arm64. See [SKROOTPRO.md](../../docs/SKROOTPRO.md)
for the authoritative ABI, packaging, prerequisite and validation contract.

```sh
export PATH=/root/.cargo/bin:$PATH
export CARGO_TARGET_DIR=/root/workspace/omk-skroot-loader-target
export OMK_SKROOT_VERSION=1.3.8-302-lite
export OMK_SKROOT_VARIANT=lite
cargo +stable fmt --all -- --check
cargo +stable clippy --target aarch64-linux-android --all-targets -- -D warnings
cargo +stable test --target aarch64-linux-android --no-run
cargo +stable build --release --target aarch64-linux-android
```

Outputs: `libomk_skroot_loader.so` and `omk-skroot-worker` in
`$CARGO_TARGET_DIR/aarch64-linux-android/release/`. The worker belongs in ZIP
`skroot/omk-skroot-worker`; only the cdylib belongs at ZIP top level.
