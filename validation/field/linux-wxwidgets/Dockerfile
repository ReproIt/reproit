# Debian trixie, not bookworm: poedit's configure demands wxWidgets 3.2.4 and
# bookworm ships 3.2.2. wxMaxima builds against the same 3.2.6, so one image
# carries both subjects and both revisions of each.
FROM debian:trixie@sha256:fac46bff2e02f51425b6e33b0e1169f55dfb053d83511ca28aa50c09fd5ed7a4

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        at-spi2-core \
        autoconf \
        automake \
        build-essential \
        cmake \
        dbus-x11 \
        gettext \
        libatk-adaptor \
        libatspi2.0-0t64 \
        libboost-iostreams-dev \
        libboost-system-dev \
        libboost-thread-dev \
        libgtkspell3-3-dev \
        libicu-dev \
        liblucene++-dev \
        libpugixml-dev \
        libsecret-1-dev \
        libtool \
        libwxgtk-webview3.2-dev \
        libwxgtk3.2-dev \
        maxima \
        ninja-build \
        nlohmann-json3-dev \
        openbox \
        pkg-config \
        python3 \
        python3-pyatspi \
        wmctrl \
        x11-utils \
        xdotool \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

COPY wxmaxima-affected /src/wxmaxima-affected
COPY wxmaxima-fixed /src/wxmaxima-fixed

RUN cmake -S /src/wxmaxima-affected -B /src/wxmaxima-affected/build \
        -G Ninja -DCMAKE_BUILD_TYPE=Release \
    && cmake --build /src/wxmaxima-affected/build --parallel 12 \
    && install -D /src/wxmaxima-affected/build/src/wxmaxima \
        /opt/reproit/wxmaxima-affected \
    && cmake -S /src/wxmaxima-fixed -B /src/wxmaxima-fixed/build \
        -G Ninja -DCMAKE_BUILD_TYPE=Release \
    && cmake --build /src/wxmaxima-fixed/build --parallel 12 \
    && install -D /src/wxmaxima-fixed/build/src/wxmaxima \
        /opt/reproit/wxmaxima-fixed \
    && rm -rf /src/wxmaxima-affected /src/wxmaxima-fixed

COPY poedit-affected /src/poedit-affected
COPY poedit-fixed /src/poedit-fixed

# The cloud accounts and language-detection integrations are the only
# network-reaching features poedit has, and both are optional. Disabling them
# keeps every run offline without touching the file viewer under test.
RUN cd /src/poedit-affected \
    && ./bootstrap > /dev/null \
    && ./configure --without-cpprest --without-cld2 > /dev/null \
    && make -j12 > /dev/null \
    && install -D /src/poedit-affected/src/poedit /opt/reproit/poedit-affected \
    && cd /src/poedit-fixed \
    && ./bootstrap > /dev/null \
    && ./configure --without-cpprest --without-cld2 > /dev/null \
    && make -j12 > /dev/null \
    && install -D /src/poedit-fixed/src/poedit /opt/reproit/poedit-fixed \
    && rm -rf /src

RUN dpkg-query -W -f='${binary:Package}\t${Version}\t${Architecture}\n' \
        | sort > /opt/reproit/build-packages.tsv \
    && { \
        g++ --version; \
        wx-config --version; \
        cmake --version; \
        python3 --version; \
    } > /opt/reproit/toolchain.txt 2>&1

COPY probe.py /opt/reproit/probe.py
COPY atspi_helpers.py /opt/reproit/atspi_helpers.py

ENTRYPOINT ["dbus-run-session", "--", "python3", "/opt/reproit/probe.py"]
