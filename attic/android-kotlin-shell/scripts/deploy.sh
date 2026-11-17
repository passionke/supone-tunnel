#!/usr/bin/env bash
# Author: kejiqing
# 经跳板 ADB 安装到 android-x86 验收机。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APK="${1:-$ROOT/app/build/outputs/apk/debug/app-debug.apk}"
SER="${ADB_SERIAL:-127.0.0.1:15555}"
JUMP="${SSH_JUMP:-passionke@10.8.0.29}"
VM_IP="${VM_IP:-192.168.3.209}"
LOCAL_ADB_PORT="${LOCAL_ADB_PORT:-15555}"

export PATH="${ANDROID_HOME:-$HOME/Library/Android/sdk}/platform-tools:$PATH"

if ! adb -s "$SER" get-state >/dev/null 2>&1; then
  ssh -f -N -o ExitOnForwardFailure=yes -o StrictHostKeyChecking=no \
    -L "${LOCAL_ADB_PORT}:${VM_IP}:5555" "$JUMP" || true
  sleep 1
  adb connect "127.0.0.1:${LOCAL_ADB_PORT}"
  SER="127.0.0.1:${LOCAL_ADB_PORT}"
fi

test -f "$APK"
adb -s "$SER" install -r -g "$APK"
PKG=top.passionke.supone_tunnel
adb -s "$SER" shell "pm grant $PKG android.permission.WRITE_SECURE_SETTINGS" || true
adb -s "$SER" shell "am force-stop $PKG; am start -n $PKG/.MainActivity --ez autostart true"
# 应用进程写 Settings.Global 在部分镜像不稳定；验收机用 root 落全局代理。
sleep 8
adb -s "$SER" shell "su 0 settings put global http_proxy 127.0.0.1:7890" || \
  adb -s "$SER" shell "settings put global http_proxy 127.0.0.1:7890"
adb -s "$SER" shell "settings get global http_proxy"
echo "installed $PKG on $SER"
