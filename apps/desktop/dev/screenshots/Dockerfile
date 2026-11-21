# An isolated X display for NSH's native Linux (GTK 4) host, so screenshots
# never touch the developer's desktop. The NSH distribution is mounted at run
# time (read-only); this image carries only the system libraries it needs:
# glibc >= 2.39, GTK >= 4.14, WebKitGTK 6.0 (the host links it), Xvfb, fonts.
FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      libgtk-4-1 libwebkitgtk-6.0-4 libgstreamer1.0-0 libgstreamer-plugins-base1.0-0 \
      xvfb xauth dbus-x11 fonts-inter fonts-dejavu-core fonts-noto-color-emoji \
      ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*
ENV GDK_BACKEND=x11 GSK_RENDERER=cairo LIBGL_ALWAYS_SOFTWARE=1 NO_AT_BRIDGE=1
