#!/usr/bin/env bash
# Author: kejiqing
# 构建 Android APK。REGION=china 走国内镜像拉依赖与 mihomo。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$ROOT/.." && pwd)"
ASSETS="$ROOT/app/src/main/assets"
REGION="${REGION:-china}"
export REGION

export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk@17}"
export PATH="$JAVA_HOME/bin:$PATH"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export ANDROID_SDK_ROOT="$ANDROID_HOME"

mkdir -p "$ASSETS"
echo "sdk.dir=$ANDROID_HOME" >"$ROOT/local.properties"

MIHOMO_ASSET="$ASSETS/mihomo-android-amd64"
if [[ ! -f "$MIHOMO_ASSET" || $(stat -f%z "$MIHOMO_ASSET" 2>/dev/null || echo 0) -lt 1000000 ]]; then
  TAG="${MIHOMO_TAG:-v1.19.33}"
  NAME="mihomo-android-amd64-${TAG}.gz"
  URL="https://github.com/MetaCubeX/mihomo/releases/download/${TAG}/${NAME}"
  if [[ "$REGION" == "china" ]]; then
    URL="https://ghfast.top/${URL}"
  fi
  echo "下载内核: $URL"
  curl -fL --retry 3 -A "supone-tunnel" -o /tmp/mihomo-android.gz "$URL"
  gunzip -c /tmp/mihomo-android.gz >"$MIHOMO_ASSET"
  chmod +x "$MIHOMO_ASSET"
fi

cd "$ROOT"
if [[ ! -f ./gradlew ]]; then
  gradle wrapper --gradle-version 8.11.1
fi

./gradlew :app:assembleDebug
APK="$ROOT/app/build/outputs/apk/debug/app-debug.apk"
echo "APK=$APK"
ls -la "$APK"
