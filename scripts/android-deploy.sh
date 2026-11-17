#!/usr/bin/env bash
# Author: kejiqing
# 构建 Tauri Android（复用 ui/）并装到 KVM android-x86。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export REGION="${REGION:-china}"
export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@17}"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export NDK_HOME="${NDK_HOME:-$ANDROID_HOME/ndk/29.0.14206865}"
export PATH="$HOME/.rustup/toolchains/1.97.1-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$JAVA_HOME/bin:$ANDROID_HOME/build-tools/36.1.0:$PATH"

cd "$ROOT"
npx tauri android build --target x86_64 --apk
UNSIGNED="$ROOT/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk"
SIGNED=/tmp/supone-tunnel-release-signed.apk
KS="${ANDROID_DEBUG_KEYSTORE:-$HOME/.android/debug.keystore}"
cp "$UNSIGNED" /tmp/supone-unsigned.apk
zipalign -f -p 4 /tmp/supone-unsigned.apk /tmp/supone-aligned.apk
apksigner sign --ks "$KS" --ks-pass pass:android --key-pass pass:android --out "$SIGNED" /tmp/supone-aligned.apk

VM_IP="${VM_IP:-192.168.3.201}"
JUMP="${SSH_JUMP:-passionke@10.8.0.29}"
rsync -avP "$SIGNED" "$JUMP:/tmp/supone-tunnel-release-signed.apk"
ssh "$JUMP" "adb connect ${VM_IP}:5555; adb -s ${VM_IP}:5555 install -r -g /tmp/supone-tunnel-release-signed.apk; adb -s ${VM_IP}:5555 shell pm grant top.passionke.supone_tunnel android.permission.WRITE_SECURE_SETTINGS || true; adb -s ${VM_IP}:5555 shell am start -n top.passionke.supone_tunnel/.MainActivity"
echo "APK=$SIGNED"
