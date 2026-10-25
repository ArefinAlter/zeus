#!/bin/bash
# Builds every Linux x64 release asset inside linux-build.Dockerfile. Sources at /src/zeus and /src/zeus-core
# (read-write: npm ci writes node_modules), output in /out. Targets go to /target (a volume) to keep the source clean.
set -euo pipefail
export CARGO_TARGET_DIR=/target
V=0.0.1a
N=linux-x64
cd /src/zeus
cargo build --release --locked -p zeus-daemon -p zeus-cli -p zeus-native-host
(cd services/pre-alpha/gateway && cargo build --release --locked)
cargo build --release --locked --manifest-path apps/desktop/Cargo.toml
# The client's node_modules come from Windows; Linux needs its own copy of the native Tauri CLI.
rm -rf /tmp/client && mkdir -p /tmp/client && cp -r apps/client/. /tmp/client/ && rm -rf /tmp/client/node_modules /tmp/client/src-tauri/target
cd /tmp/client
npm ci --no-audit --no-fund
npm run build
npx tauri build --bundles deb,appimage
cd /out
rm -rf "zeus-cli-$N" "zeus-desktop-$N"
mkdir -p "zeus-cli-$N" "zeus-desktop-$N"
cp /target/release/zeus /target/release/zeusd /target/release/zeus-native-host /target/release/zeus-web-bridge "zeus-cli-$N/"
cp /src/zeus/LICENSE /src/zeus/NOTICE "zeus-cli-$N/"
tar -czf "zeus-cli-$V-$N.tar.gz" "zeus-cli-$N"
cp /target/release/zeus-desktop "zeus-desktop-$N/"
cp /src/zeus/LICENSE /src/zeus/NOTICE "zeus-desktop-$N/"
tar -czf "zeus-desktop-$V-$N.tar.gz" "zeus-desktop-$N"
cp /target/release/bundle/deb/*.deb /target/release/bundle/appimage/*.AppImage /out/
rm -rf "zeus-cli-$N" "zeus-desktop-$N"
ls -la /out
