# Android cross-build environment for S31. Source it: `. android/env.sh`.
export ANDROID_HOME=${ANDROID_HOME:-$HOME/Android/Sdk}
export NDK=${NDK:-$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}
export ANDROID_NDK_HOME=$NDK
export PATH=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin:$PATH
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=aarch64-linux-android29-clang
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=x86_64-linux-android29-clang
export CC_aarch64_linux_android=aarch64-linux-android29-clang
export CC_x86_64_linux_android=x86_64-linux-android29-clang
export AR_aarch64_linux_android=llvm-ar
export AR_x86_64_linux_android=llvm-ar
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$HOME/.cache/s31-target}
