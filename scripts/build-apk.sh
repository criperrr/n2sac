#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JAVA17_HOME="${JAVA_HOME_17:-/usr/lib/jvm/java-17-openjdk}"

echo "[build-apk] Building Vite web bundle..."
(cd "${ROOT_DIR}/app" && npm run build && npx cap sync android)

echo "[build-apk] Compiling Android debug APK with Java 17..."
(cd "${ROOT_DIR}/app/android" && JAVA_HOME="${JAVA17_HOME}" ./gradlew assembleDebug)

if command -v adb >/dev/null 2>&1 && adb get-state >/dev/null 2>&1; then
  echo "[build-apk] Installing APK via adb..."
  adb install -r "${ROOT_DIR}/app/android/app/build/outputs/apk/debug/app-debug.apk"
  echo "[build-apk] Installed successfully!"
else
  echo "[build-apk] APK ready at ${ROOT_DIR}/app/android/app/build/outputs/apk/debug/app-debug.apk (no adb device connected)"
fi
