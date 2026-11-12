# shellcheck disable=SC2034
SKIPUNZIP=1

SONAME="Oh My Keymint"
MIN_SDK=31

case "${BOOTMODE:-}" in
  true|1) ;;
  *) abort "! Install from a running Android module manager" ;;
esac

# Both KernelSU and APatch export compatibility MAGISK_VER values. Test their
# documented identity flags first; framework version numbers are not features.
if [ "${APATCH:-}" = true ]; then
  ui_print "- Installing from APatch"
elif [ "${KSU:-}" = true ]; then
  ui_print "- Installing from KernelSU"
elif [ -r /data/adb/magisk/util_functions.sh ] || command -v magisk >/dev/null 2>&1; then
  ui_print "- Installing from Magisk"
else
  abort "! Expected a KernelSU, APatch or Magisk module installer"
fi

case "$ARCH" in
  arm64|arm64-v8a) BINARY_ABI=arm64-v8a ;;
  x64|x86_64) BINARY_ABI=x86_64 ;;
  *) abort "! Unsupported platform: $ARCH" ;;
esac
case "$API" in
  ''|*[!0-9]*) abort "! Missing Android API level" ;;
esac
[ "$API" -ge "$MIN_SDK" ] || abort "! Android 12 or newer is required"
ui_print "- Device platform: $BINARY_ABI (API $API)"

ui_print "- Extracting verifier"
unzip -o "$ZIPFILE" 'verify.sh' -d "$TMPDIR" >&2
[ -f "$TMPDIR/verify.sh" ] || abort "! Missing verify.sh"
. "$TMPDIR/verify.sh"
extract "$ZIPFILE" 'customize.sh' "$TMPDIR/.vunzip"
extract "$ZIPFILE" 'verify.sh' "$TMPDIR/.vunzip"
extract "$ZIPFILE" 'payload.manifest' "$MODPATH"

# The build creates this complete, hash-verified inventory for each variant.
# Optional PIF/SOTER files are installed only when actually shipped.
while IFS= read -r asset || [ -n "$asset" ]; do
  case "$asset" in
    ''|/*|../*|*/../*|*/..|./*|*/./*|*/.|*//*|*\\*|*[!a-zA-Z0-9_./+-]*)
      abort "! Invalid payload path: $asset" ;;
    META-INF/*|customize.sh|verify.sh|payload.manifest|*.sha256)
      abort "! Unexpected payload entry: $asset" ;;
  esac
  extract "$ZIPFILE" "$asset" "$MODPATH"
done < "$MODPATH/payload.manifest"

for required in module.prop post-fs-data.sh service.sh daemon daemon-injector \
  uninstall.sh sepolicy.rule r1-build.json arm64-v8a/r1-helper \
  "libs/$BINARY_ABI/keymint" "libs/$BINARY_ABI/inject"
do
  [ -f "$MODPATH/$required" ] || abort "! Missing required payload: $required"
done

VERSION=$(grep_prop version "$MODPATH/module.prop")
ui_print "- Installing $SONAME $VERSION"
set_perm_recursive "$MODPATH" 0 0 0755 0644
chmod 0755 "$MODPATH/daemon" "$MODPATH/daemon-injector" \
  "$MODPATH/post-fs-data.sh" "$MODPATH/service.sh" "$MODPATH/uninstall.sh" \
  "$MODPATH/libs/$BINARY_ABI/keymint" "$MODPATH/libs/$BINARY_ABI/inject" \
  "$MODPATH/arm64-v8a/r1-helper"
for library in "$MODPATH/zygisk/$BINARY_ABI.so" "$MODPATH/libs/$BINARY_ABI/libomk_engineer.so"; do
  [ ! -f "$library" ] || chmod 0755 "$library"
done

STATE_DIR=/data/adb/omk
mkdir -p "$STATE_DIR"
rm -f "$STATE_DIR/restart.keymint" "$STATE_DIR/restart.injector" "$STATE_DIR/restart.all"
rm -f "$STATE_DIR/keymint" "$STATE_DIR/inject" "$STATE_DIR/injector"
if [ ! -e "$STATE_DIR/omkdata" ] && [ ! -L "$STATE_DIR/omkdata" ]; then
  ln -s /data/misc/keystore/omk "$STATE_DIR/omkdata"
fi
