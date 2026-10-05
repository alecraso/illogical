#!/bin/sh
# Builds the S31 test app without Gradle: javac, d8, aapt2, apksigner.
# Usage: build-apk.sh ABI RUST_TARGET   (x86_64 x86_64-linux-android, arm64-v8a aarch64-linux-android)
set -eu
here=$(cd "$(dirname "$0")" && pwd)
. "$here/env.sh"
abi=$1; target=$2
bt=$ANDROID_HOME/build-tools/36.0.0
jar=$ANDROID_HOME/platforms/android-36/android.jar
out=$CARGO_TARGET_DIR/s31-apk/$abi
rm -rf "$out" && mkdir -p "$out/classes" "$out/lib/$abi"
javac -source 11 -target 11 -classpath "$jar" -d "$out/classes" $(find "$here/app/src" -name '*.java') 2>&1 | grep -v warning || true
"$bt/d8" --min-api 29 --lib "$jar" --output "$out" $(find "$out/classes" -name '*.class')
"$bt/aapt2" link -o "$out/base.apk" -I "$jar" --manifest "$here/app/AndroidManifest.xml"
cp "$CARGO_TARGET_DIR/$target/release/illogicald" "$out/lib/$abi/libillogicald.so"
cp "$CARGO_TARGET_DIR/$target/release/illogical" "$out/lib/$abi/libillogical.so"
# Extra native libs (proot, its loader and libraries: see README) if fetched.
[ -d "$HOME/.cache/s31-dl/extra/$abi" ] && cp "$HOME/.cache/s31-dl/extra/$abi"/*.so "$out/lib/$abi/"
(cd "$out" && zip -q base.apk classes.dex && zip -q -0 base.apk lib/$abi/*.so)
"$bt/zipalign" -f -p 4 "$out/base.apk" "$out/aligned.apk"
ks=$HOME/.android/s31-debug.keystore
[ -f "$ks" ] || keytool -genkeypair -keystore "$ks" -storepass android -alias d -keyalg RSA -validity 10000 -dname CN=s31 >/dev/null 2>&1
"$bt/apksigner" sign --ks "$ks" --ks-pass pass:android --out "$out/s31.apk" "$out/aligned.apk"
echo "$out/s31.apk"
