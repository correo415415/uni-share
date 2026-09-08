#!/usr/bin/env bash
# ============================================================
#  uni-share — Android build script (Linux / macOS / WSL)
# ============================================================
#  Cross-compiles the Rust engine (`libuni_share.so`) for the 3 Android ABIs
#  and drops it under android/app/src/main/jniLibs/<abi>/, then optionally
#  assembles the APK with Gradle.
#
#  One-time setup:
#    1. Rust (stable) + Android targets:
#         rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
#    2. Android NDK r26+ (sdkmanager --install "ndk;26.1.10909125") and
#         export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/26.1.10909125
#    3. cargo install cargo-ndk
#    4. Android SDK + JDK 17 — only for the APK step.
#
#  Usage:
#    android/build-android.sh                 # .so for the 3 ABIs (release)
#    android/build-android.sh --apk           # + debug APK
#    android/build-android.sh --release       # + signed release APK (local keystore)
#    ABIS="arm64-v8a" android/build-android.sh   # subset of ABIs
# ============================================================
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
JNILIBS="$ROOT/android/app/src/main/jniLibs"
ABIS="${ABIS:-arm64-v8a armeabi-v7a x86_64}"
PROFILE="${PROFILE:-release}"
# Android API 24 (7.0): first level with a usable mDNS/multicast stack and TLS 1.2+ by default.
API="${ANDROID_API:-24}"

command -v cargo >/dev/null || { echo "ERROR: cargo not found. Install Rust."; exit 1; }
command -v cargo-ndk >/dev/null 2>&1 || { echo "ERROR: cargo-ndk not installed. Run: cargo install cargo-ndk"; exit 1; }
if [[ -z "${ANDROID_NDK_HOME:-}" && -z "${NDK_HOME:-}" && -z "${ANDROID_NDK_ROOT:-}" ]]; then
    echo "ERROR: ANDROID_NDK_HOME (or NDK_HOME / ANDROID_NDK_ROOT) is not set."
    echo "       e.g. export ANDROID_NDK_HOME=\$HOME/Android/Sdk/ndk/26.1.10909125"
    exit 1
fi

echo "==> Rust Android targets"
for abi in $ABIS; do
    case "$abi" in
        arm64-v8a)   rustup target add aarch64-linux-android   >/dev/null ;;
        armeabi-v7a) rustup target add armv7-linux-androideabi >/dev/null ;;
        x86_64)      rustup target add x86_64-linux-android    >/dev/null ;;
        x86)         rustup target add i686-linux-android      >/dev/null ;;
        *) echo "Unknown ABI: $abi"; exit 1 ;;
    esac
done

echo "==> Cross-compiling libuni_share.so (profile=$PROFILE, api=$API, abis=$ABIS)"
cd "$ROOT"
TARGETS=(); for abi in $ABIS; do TARGETS+=(-t "$abi"); done
# cargo-ndk sets CC/AR/linker/sysroot for each target; the lib is a cdylib (Cargo.toml).
# Desktop-only crates (arboard, notify-rust, rustls-platform-verifier, slint) are
# excluded via target-specific dependencies / features, so no extra flags are needed.
cargo ndk "${TARGETS[@]}" -P "$API" -o "$JNILIBS" build --lib --profile "$PROFILE"

echo ""
echo "==> Native libs produced:"
find "$JNILIBS" -name "*.so" -exec ls -lh {} \;

case "${1:-}" in
    --apk|--debug)
        echo ""; echo "==> Assembling debug APK"
        cd "$ROOT/android" && ./gradlew assembleDebug
        echo "APK: android/app/build/outputs/apk/debug/app-debug.apk"
        ;;
    --release)
        KEYSTORE="$ROOT/android/release.keystore"
        KS_PASS="${UNISHARE_KEYSTORE_PASS:-uni-share}"
        KS_ALIAS="${UNISHARE_KEY_ALIAS:-unishare}"
        if [[ ! -f "$KEYSTORE" ]]; then
            if command -v keytool >/dev/null 2>&1; then
                echo "==> Generating local release keystore (first run)"
                keytool -genkeypair -v -keystore "$KEYSTORE" -alias "$KS_ALIAS" -keyalg RSA -keysize 2048 -validity 10000 \
                    -storepass "$KS_PASS" -keypass "$KS_PASS" -dname "CN=uni-share, OU=dev, O=uni-share, L=local, S=local, C=ES"
            else
                echo "WARN: keytool not found (install a JDK). Building UNSIGNED release."
            fi
        fi
        echo "==> Assembling release APK"
        cd "$ROOT/android" && UNISHARE_KEYSTORE_PASS="$KS_PASS" UNISHARE_KEY_ALIAS="$KS_ALIAS" ./gradlew assembleRelease
        if [[ -f "$KEYSTORE" ]]; then
            echo "Signed APK: android/app/build/outputs/apk/release/app-release.apk  (adb install -r …)"
        else
            echo "Unsigned APK: android/app/build/outputs/apk/release/app-release-unsigned.apk — sign with apksigner"
        fi
        ;;
    "")
        echo ""; echo "Native libs ready. Next: android/build-android.sh --apk | --release"
        ;;
    *)
        echo "Unknown argument: $1"; echo "Usage: $0 [--apk | --release]"; exit 1 ;;
esac
