#!/bin/sh
# Builds the dex companion APK with the plain SDK tools (no Gradle).
#   ANDROID_HOME=/path/to/sdk ./build.sh   ->  build/dex-companion.apk
set -eu

cd "$(dirname "$0")"
SDK=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/Sdk}}
BT=$(ls -d "$SDK"/build-tools/* | sort -V | tail -n 1)
PLATFORM=$(ls -d "$SDK"/platforms/android-* | sort -V | tail -n 1)
JAR="$PLATFORM/android.jar"
KEYSTORE=${DEX_KEYSTORE:-$HOME/.android/debug.keystore}

rm -rf build
mkdir -p build/classes build/dex

"$BT/aapt2" link -o build/base.apk -I "$JAR" --manifest AndroidManifest.xml \
    --min-sdk-version 26 --target-sdk-version 35 --version-code 1 --version-name 0.1
javac --release 17 -nowarn -classpath "$JAR" -d build/classes $(find src -name '*.java')
"$BT/d8" --release --min-api 26 --lib "$JAR" --output build/dex $(find build/classes -name '*.class')

cp build/base.apk build/unsigned.apk
(cd build/dex && zip -q ../unsigned.apk classes.dex)
"$BT/zipalign" -f 4 build/unsigned.apk build/aligned.apk
"$BT/apksigner" sign --ks "$KEYSTORE" --ks-pass pass:android --out build/dex-companion.apk build/aligned.apk
echo "built companion/build/dex-companion.apk"
