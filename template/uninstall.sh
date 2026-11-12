#!/system/bin/sh

# The manager calls this hook only when removing the module. Use the same
# bounded cleanup for KernelSU, APatch and Magisk; disable/update do not call it.
remove_omk_directory() {
  target="$1"
  case "$target" in
    /data/adb/omk|/data/misc/keystore/omk) ;;
    *)
      echo "[oh_my_keymint] refusing unexpected cleanup path: $target" >&2
      return 1
      ;;
  esac

  if [ -e "$target" ] || [ -L "$target" ]; then
    if ! rm -rf "$target"; then
      echo "[oh_my_keymint] failed to remove $target" >&2
      return 1
    fi
  fi
}

status=0
remove_omk_directory /data/adb/omk || status=1
remove_omk_directory /data/misc/keystore/omk || status=1
exit "$status"
