# Debian trixie: gnome-text-editor and gnome-clocks at these revisions need
# gtk4 >= 4.17.3 and libadwaita >= 1.6, which bookworm's 4.8 cannot satisfy.
FROM debian:trixie@sha256:fac46bff2e02f51425b6e33b0e1169f55dfb053d83511ca28aa50c09fd5ed7a4

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        at-spi2-core \
        build-essential \
        dbus-x11 \
        desktop-file-utils \
        gettext \
        itstool \
        libadwaita-1-dev \
        libatspi2.0-0t64 \
        libeditorconfig-dev \
        libgeoclue-2-dev \
        libgeocode-glib-dev \
        libgirepository1.0-dev \
        libgnome-desktop-4-dev \
        libgtk-4-dev \
        libgtksourceview-5-dev \
        libgweather-4-dev \
        libpango1.0-dev \
        libspelling-1-dev \
        meson \
        ninja-build \
        openbox \
        pkg-config \
        python3 \
        python3-pyatspi \
        valac \
        wmctrl \
        x11-utils \
        xdotool \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

COPY text-editor-affected /src/text-editor-affected
COPY text-editor-fixed /src/text-editor-fixed
COPY clocks-affected /src/clocks-affected
COPY clocks-fixed /src/clocks-fixed

# Each build gets its own prefix so the two revisions of one application never
# share a schema or a resource bundle. The schemas are compiled into that
# prefix because the probe points GSETTINGS_SCHEMA_DIR at it rather than
# installing anything into the image's shared directories.
RUN for pair in text-editor-affected text-editor-fixed clocks-affected clocks-fixed; \
    do \
        meson setup "/src/$pair/build" "/src/$pair" \
            --prefix "/opt/reproit/$pair" -Dbuildtype=release > "/tmp/$pair.log" 2>&1 \
        && ninja -C "/src/$pair/build" install >> "/tmp/$pair.log" 2>&1 \
        && glib-compile-schemas "/opt/reproit/$pair/share/glib-2.0/schemas" \
        || { echo "FAILED $pair"; tail -40 "/tmp/$pair.log"; exit 1; }; \
    done \
    && rm -rf /src

RUN dpkg-query -W -f='${binary:Package}\t${Version}\t${Architecture}\n' \
        | sort > /opt/reproit/build-packages.tsv \
    && { \
        cc --version; \
        pkg-config --modversion gtk4; \
        pkg-config --modversion libadwaita-1; \
        meson --version; \
        python3 --version; \
    } > /opt/reproit/toolchain.txt 2>&1

COPY probe.py /opt/reproit/probe.py
COPY atspi_helpers.py /opt/reproit/atspi_helpers.py

ENTRYPOINT ["dbus-run-session", "--", "python3", "/opt/reproit/probe.py"]
