@echo off
REM ============================================================
REM  uni-share - Android build script (Windows)
REM ============================================================
REM  Cross-compiles libuni_share.so for 3 Android ABIs into
REM  android\app\src\main\jniLibs\ and optionally builds the APK.
REM
REM  One-time setup:
REM    rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
REM    setx ANDROID_NDK_HOME "C:\Android\Sdk\ndk\26.1.10909125"   (re-open cmd)
REM    cargo install cargo-ndk
REM    Android SDK + JDK 17 (APK step only)
REM
REM  Usage:  android\build-android.bat [--apk | --release]
REM ============================================================
setlocal enabledelayedexpansion
set "ROOT=%~dp0.."
set "JNILIBS=%ROOT%\android\app\src\main\jniLibs"
if "%ANDROID_API%"=="" set ANDROID_API=24

where cargo >nul 2>nul || (echo ERROR: cargo not found. Install Rust. & exit /b 1)
where cargo-ndk >nul 2>nul || (echo ERROR: cargo-ndk not installed. Run: cargo install cargo-ndk & exit /b 1)
if "%ANDROID_NDK_HOME%"=="" if "%NDK_HOME%"=="" if "%ANDROID_NDK_ROOT%"=="" (
    echo ERROR: ANDROID_NDK_HOME is not set. Example:
    echo        setx ANDROID_NDK_HOME "C:\Android\Sdk\ndk\26.1.10909125"
    exit /b 1
)

echo ==^> Rust Android targets
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android >nul

echo ==^> Cross-compiling libuni_share.so (release, api=%ANDROID_API%)
cd /d "%ROOT%"
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -P %ANDROID_API% -o "%JNILIBS%" build --lib --release
if errorlevel 1 exit /b 1

echo.
echo ==^> Native libs produced:
dir /s /b "%JNILIBS%\*.so"

if "%~1"=="--apk" goto apk
if "%~1"=="--debug" goto apk
if "%~1"=="--release" goto release
echo.
echo Native libs ready. Next: android\build-android.bat --apk ^| --release
exit /b 0

:apk
echo ==^> Assembling debug APK
cd /d "%ROOT%\android" && call gradlew.bat assembleDebug
echo APK: android\app\build\outputs\apk\debug\app-debug.apk
exit /b 0

:release
set "KEYSTORE=%ROOT%\android\release.keystore"
if "%UNISHARE_KEYSTORE_PASS%"=="" set UNISHARE_KEYSTORE_PASS=uni-share
if "%UNISHARE_KEY_ALIAS%"=="" set UNISHARE_KEY_ALIAS=unishare
if not exist "%KEYSTORE%" (
    where keytool >nul 2>nul && (
        echo ==^> Generating local release keystore
        keytool -genkeypair -v -keystore "%KEYSTORE%" -alias %UNISHARE_KEY_ALIAS% -keyalg RSA -keysize 2048 -validity 10000 -storepass %UNISHARE_KEYSTORE_PASS% -keypass %UNISHARE_KEYSTORE_PASS% -dname "CN=uni-share, OU=dev, O=uni-share, L=local, S=local, C=ES"
    ) || echo WARN: keytool not found. Building UNSIGNED release.
)
echo ==^> Assembling release APK
cd /d "%ROOT%\android" && call gradlew.bat assembleRelease
echo APK: android\app\build\outputs\apk\release\
exit /b 0
