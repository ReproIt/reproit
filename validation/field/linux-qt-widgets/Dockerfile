FROM debian:bookworm@sha256:9344f8b8992482f80cba753f323adeaf17690076c095ccff6cc9536be98185dc

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        at-spi2-core \
        build-essential \
        cmake \
        dbus-x11 \
        libargon2-dev \
        libatspi2.0-0 \
        libbotan-2-dev \
        libkeyutils-dev \
        libminizip-dev \
        libpcsclite-dev \
        libqrencode-dev \
        libqt6svg6-dev \
        libreadline-dev \
        libsodium-dev \
        libusb-1.0-0-dev \
        libx11-dev \
        libxi-dev \
        libxkbcommon-x11-0 \
        libxtst-dev \
        libzxcvbn-dev \
        ninja-build \
        openbox \
        pkg-config \
        python3 \
        python3-pyatspi \
        qt6-base-dev \
        qt6-base-dev-tools \
        qt6-tools-dev \
        qt6-tools-dev-tools \
        wmctrl \
        x11-utils \
        xdotool \
        xvfb \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

COPY keepassxc-affected /src/keepassxc-affected
COPY keepassxc-fixed /src/keepassxc-fixed

RUN apt-get update \
    && apt-get install -y --no-install-recommends libxkbcommon-dev \
    && rm -rf /var/lib/apt/lists/*

RUN cmake -S /src/keepassxc-affected -B /src/keepassxc-affected/build \
        -G Ninja \
        -DCMAKE_BUILD_TYPE=Release \
        -DWITH_TESTS=OFF \
        -DWITH_X11=OFF \
        -DKPXC_MINIMAL=ON \
        -DKPXC_FEATURE_NETWORK=OFF \
        -DKPXC_FEATURE_DOCS=OFF \
    && cmake --build /src/keepassxc-affected/build --parallel 2 \
    && install -D /src/keepassxc-affected/build/src/keepassxc \
        /opt/reproit/keepassxc-affected \
    && install -D /src/keepassxc-affected/build/src/cli/keepassxc-cli \
        /opt/reproit/keepassxc-cli-affected \
    && cmake -S /src/keepassxc-fixed -B /src/keepassxc-fixed/build \
        -G Ninja \
        -DCMAKE_BUILD_TYPE=Release \
        -DWITH_TESTS=OFF \
        -DWITH_X11=OFF \
        -DKPXC_MINIMAL=ON \
        -DKPXC_FEATURE_NETWORK=OFF \
        -DKPXC_FEATURE_DOCS=OFF \
    && cmake --build /src/keepassxc-fixed/build --parallel 2 \
    && install -D /src/keepassxc-fixed/build/src/keepassxc \
        /opt/reproit/keepassxc-fixed \
    && install -D /src/keepassxc-fixed/build/src/cli/keepassxc-cli \
        /opt/reproit/keepassxc-cli-fixed \
    && rm -rf /src

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libqt5svg5-dev \
        libqt5x11extras5-dev \
        qt5-qmake \
        qtbase5-dev \
        qttools5-dev-tools \
    && rm -rf /var/lib/apt/lists/*

COPY qview-affected /src5/qview-affected
COPY qview-fixed /src5/qview-fixed

RUN /usr/lib/qt5/bin/qmake \
        /src5/qview-affected/qView.pro \
        -o /src5/qview-affected/Makefile \
    && make -C /src5/qview-affected -j2 \
    && install -D /src5/qview-affected/bin/qview \
        /opt/reproit/qview-affected \
    && /usr/lib/qt5/bin/qmake \
        /src5/qview-fixed/qView.pro \
        -o /src5/qview-fixed/Makefile \
    && make -C /src5/qview-fixed -j2 \
    && install -D /src5/qview-fixed/bin/qview \
        /opt/reproit/qview-fixed \
    && rm -rf /src5

RUN dpkg-query -W -f='${binary:Package}\t${Version}\t${Architecture}\n' \
        | sort > /opt/reproit/build-packages.tsv \
    && { \
        g++ --version; \
        qmake6 -v; \
        /usr/lib/qt5/bin/qmake -v; \
        cmake --version; \
        python3 --version; \
    } > /opt/reproit/toolchain.txt 2>&1

COPY probe.py /opt/reproit/probe.py
COPY atspi_helpers.py /opt/reproit/atspi_helpers.py

ENTRYPOINT ["dbus-run-session", "--", "python3", "/opt/reproit/probe.py"]
