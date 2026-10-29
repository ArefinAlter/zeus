# Runtime image for zeusd, zeus, zeus-native-host, zeus-web-bridge and the janitor. The binaries are built beforehand (Linux x64,
# glibc, Debian bookworm) and copied in; this image compiles nothing.
FROM debian:bookworm-slim
# zeusd's HTTP client loads the system's root certificates at start, even for plain-http fixture hosts.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
RUN useradd --uid 10001 --create-home --home-dir /home/zeus zeus \
 && mkdir -p /state /run/zeus /data/downloads /bridge \
 && chown zeus:zeus /state /run/zeus /data/downloads /bridge \
 && chmod 700 /state /run/zeus
COPY bin/zeusd bin/zeus bin/zeus-native-host bin/zeus-web-bridge /usr/local/bin/
COPY start-zeusd.sh start-bridge.sh janitor.sh /opt/zeus/
RUN chmod 755 /usr/local/bin/zeusd /usr/local/bin/zeus /usr/local/bin/zeus-native-host /usr/local/bin/zeus-web-bridge /opt/zeus/*.sh
USER zeus
