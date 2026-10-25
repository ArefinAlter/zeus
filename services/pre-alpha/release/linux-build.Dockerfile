# Linux x64 build environment for the pre-alpha release assets (Debian bookworm, glibc 2.36): zeusd, zeus,
# zeus-native-host, zeus-web-bridge, the Zeus client (Tauri: .deb and .AppImage) and the real-daemon desktop client.
#
#   docker build -t zeus-linux-builder -f services/pre-alpha/release/linux-build.Dockerfile services/pre-alpha/release
#   docker run --rm -v <zeus>:/src/zeus -v <zeus-core>:/src/zeus-core -v <out>:/out zeus-linux-builder \
#       bash /src/zeus/services/pre-alpha/release/build-linux.sh
FROM rust:1-bookworm
# dpkg's fsyncs are very slow on Docker Desktop's virtual disk; a throwaway build image does not need them.
RUN echo force-unsafe-io > /etc/dpkg/dpkg.cfg.d/unsafe-io \
 && apt-get update \
 && apt-get install -y --no-install-recommends libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
      librsvg2-dev libdbus-1-dev libssl-dev pkg-config patchelf file xz-utils ca-certificates curl \
 && curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
 && apt-get install -y --no-install-recommends nodejs \
 && rm -rf /var/lib/apt/lists/*
ENV CARGO_BUILD_JOBS=4 APPIMAGE_EXTRACT_AND_RUN=1 NO_STRIP=1
