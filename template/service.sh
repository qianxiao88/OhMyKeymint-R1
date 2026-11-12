#!/system/bin/sh

MODDIR=${0%/*}
STATE_DIR=/data/adb/omk

mkdir -p "$STATE_DIR"

# KernelSU late-load omits post-fs-data. Initialize the same module-local
# assets before starting daemons when that documented stage was skipped.
if [ "${KSU_LATE_LOAD:-}" = 1 ] || [ ! -d /data/misc/keystore/omk/data ]; then
  sh "$MODDIR/post-fs-data.sh" || exit 1
fi

pid_matches_script() {
  pid=$1
  script=$2
  [ -r "/proc/$pid/cmdline" ] || return 1
  cmdline=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)
  echo "$cmdline" | grep -F "$script" >/dev/null 2>&1
}

start_daemon() {
  script=$1
  pidfile=$2

  if [ -f "$pidfile" ]; then
    pid=$(cat "$pidfile" 2>/dev/null)
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && pid_matches_script "$pid" "$script"; then
      return 0
    fi
    rm -f "$pidfile"
  fi

  sh "$script" &
  pid=$!
  echo $pid > "$pidfile"
  sleep 1
  if ! kill -0 "$pid" 2>/dev/null || ! pid_matches_script "$pid" "$script"; then
    rm -f "$pidfile"
    return 1
  fi
  return 0
}

# BEGIN R1 BOOT HOOK
if [ -f "$MODDIR/r1-build.json" ]; then
  R1_HELPER="$MODDIR/arm64-v8a/r1-helper"
  [ -x "$R1_HELPER" ] || { echo "[R1] missing executable helper" >&2; exit 1; }
  "$R1_HELPER" monitor &
fi
# END R1 BOOT HOOK

start_daemon "$MODDIR/daemon" "$STATE_DIR/keymint-daemon.pid"
start_daemon "$MODDIR/daemon-injector" "$STATE_DIR/injector-daemon.pid"
# Legacy property adjustments use a verified resetprop interface supplied by
# the manager. The Rust date path independently resolves the same capability.
RESETPROP_BIN=
RESETPROP_PREFIX=
probe_resetprop() {
  candidate=$1
  prefix=$2
  [ -x "$candidate" ] || return 1
  expected=$(getprop ro.build.version.sdk 2>/dev/null)
  [ -n "$expected" ] || return 1
  if [ -n "$prefix" ]; then
    actual=$("$candidate" "$prefix" -n ro.build.version.sdk 2>/dev/null) || return 1
  else
    actual=$("$candidate" -n ro.build.version.sdk 2>/dev/null) || return 1
  fi
  [ "$actual" = "$expected" ] || return 1
  RESETPROP_BIN=$candidate
  RESETPROP_PREFIX=$prefix
}
for candidate in /data/adb/ksu/bin/resetprop /data/adb/magisk/resetprop \
  /data/adb/ap/bin/resetprop
do
  probe_resetprop "$candidate" "" && break
done
if [ -z "$RESETPROP_BIN" ]; then
  probe_resetprop /data/adb/ksud resetprop || probe_resetprop /data/adb/apd resetprop || true
fi
if [ -z "$RESETPROP_BIN" ]; then
  candidate=$(command -v resetprop 2>/dev/null)
  [ -z "$candidate" ] || probe_resetprop "$candidate" ""
fi
reset_prop() {
  [ -n "$RESETPROP_BIN" ] || return 1
  if [ -n "$RESETPROP_PREFIX" ]; then
    "$RESETPROP_BIN" "$RESETPROP_PREFIX" "$@"
  else
    "$RESETPROP_BIN" "$@"
  fi
}

check_reset_prop() {
  local NAME="$1"
  local EXPECTED="$2"
  local VALUE
  VALUE=$(reset_prop "$NAME")
  [ -z "$VALUE" ] || [ "$VALUE" = "$EXPECTED" ] || reset_prop "$NAME" "$EXPECTED"
}

check_missing_prop() {
  local NAME="$1"
  local EXPECTED="$2"
  local VALUE
  VALUE=$(reset_prop "$NAME")
  [ -z "$VALUE" ] && reset_prop "$NAME" "$EXPECTED"
}

check_missing_match_prop() {
  local NAME="$1"
  local EXPECTED="$2"
  local VALUE
  VALUE=$(reset_prop "$NAME")
  [ -z "$VALUE" ] || [ "$VALUE" = "$EXPECTED" ] || reset_prop "$NAME" "$EXPECTED"
  [ -z "$VALUE" ] && reset_prop "$NAME" "$EXPECTED"
}

contains_reset_prop() {
  local NAME="$1"
  local CONTAINS="$2"
  local NEWVAL="$3"
  case "$(reset_prop "$NAME")" in
    *"$CONTAINS"*) reset_prop "$NAME" "$NEWVAL" ;;
  esac
}

sleep 20

if [ -n "$RESETPROP_BIN" ]; then
  reset_prop -w sys.boot_completed 0
  check_missing_prop "ro.boot.vbmeta.invalidate_on_error" "yes"
  check_reset_prop "ro.boot.vbmeta.avb_version" "1.2"
  check_reset_prop "ro.boot.vbmeta.hash_alg" "sha256"
  check_reset_prop "ro.boot.vbmeta.size" "19968"
  check_missing_match_prop "ro.boot.vbmeta.device_state" "locked"
  check_missing_match_prop "ro.boot.verifiedbootstate" "green"
  check_reset_prop "ro.boot.flash.locked" "1"
  check_reset_prop "ro.boot.veritymode" "enforcing"
  check_reset_prop "ro.boot.warranty_bit" "0"
  check_reset_prop "ro.warranty_bit" "0"
  check_reset_prop "ro.debuggable" "0"
  check_reset_prop "ro.force.debuggable" "0"
  check_reset_prop "ro.secure" "1"
  check_reset_prop "ro.adb.secure" "1"
  check_reset_prop "ro.build.type" "user"
  check_reset_prop "ro.build.tags" "release-keys"
  check_reset_prop "ro.vendor.boot.warranty_bit" "0"
  check_reset_prop "ro.vendor.warranty_bit" "0"
  check_missing_match_prop "vendor.boot.vbmeta.device_state" "locked"
  check_missing_match_prop "vendor.boot.verifiedbootstate" "green"
  check_reset_prop "sys.oem_unlock_allowed" "0"
  check_reset_prop "ro.secureboot.lockstate" "locked"
  check_reset_prop "ro.boot.realmebootstate" "green"
  check_reset_prop "ro.boot.realme.lockstate" "1"
  check_reset_prop "ro.crypto.state" "encrypted"

  # Hide adb debugging traces.
  reset_prop "sys.usb.adb.disabled" " "
  # Hide recovery boot mode.
  contains_reset_prop "ro.bootmode" "recovery" "unknown"
  contains_reset_prop "ro.boot.bootmode" "recovery" "unknown"
  contains_reset_prop "vendor.boot.bootmode" "recovery" "unknown"
fi

#Hide Lsposed Logd
setprop persist.logd.size ""
setprop persist.logd.size.crash ""
setprop persist.logd.size.system ""
setprop persist.logd.size.main ""

# Hide cloudphone detection.
if [ -n "$RESETPROP_BIN" ] && [ -n "$(reset_prop ro.kernel.qemu)" ]; then
  reset_prop ro.kernel.qemu ""
fi
