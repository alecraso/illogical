#!/bin/sh
# Fetch the code-server release editor blocks run (crates/daemon/src/editor/
# server.rs: VERSION and SHA256) into .run/cache, for tests/code-server.spec.ts.
set -eu
v=4.140.0
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) p=macos-arm64 sum=82c7144406ac31c373acfa786b6705c7c5463d895f728fde2cb94402b945b301 ;;
  Darwin-x86_64) p=macos-amd64 sum=a5393b6eed4aa68b084e724c3c565f805abd996c609356043119f0323e40cf52 ;;
  Linux-x86_64) p=linux-amd64 sum=864c5d01c808ade57e4d12c708717be7a187219fded60428f263b9e2da9f6b48 ;;
  Linux-aarch64) p=linux-arm64 sum=ae4b07153f2037b06d24749bc8004221fcbf3ffe317038401be0452f541bf200 ;;
  *) echo "no code-server release for this platform" >&2; exit 1 ;;
esac
mkdir -p .run/cache && cd .run/cache
[ -x "code-server-$v-$p/bin/code-server" ] && exit 0
curl -fsSL -o cs.tgz "https://github.com/coder/code-server/releases/download/v$v/code-server-$v-$p.tar.gz"
echo "$sum  cs.tgz" | shasum -a 256 -c -
tar xzf cs.tgz && rm cs.tgz
