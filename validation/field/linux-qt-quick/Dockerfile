# Debian trixie: these revisions need Qt 6 and KF 6.13, and the Qt AT-SPI
# bridge is built into QtGui here rather than shipped as a plugin.
FROM debian:trixie@sha256:fac46bff2e02f51425b6e33b0e1169f55dfb053d83511ca28aa50c09fd5ed7a4

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        at-spi2-core \
        bison \
        build-essential \
        flex \
        cmake \
        dbus-x11 \
        extra-cmake-modules \
        gettext \
        libkf6baloo-dev \
        libkf6codecs-dev \
        libkf6filemetadata-dev \
        libkf6kio-dev \
        libkf6qqc2desktopstyle-dev \
        libkf6xmlgui-dev \
        libkf6colorscheme-dev \
        libkf6config-dev \
        libkf6coreaddons-dev \
        libkf6crash-dev \
        libkf6dbusaddons-dev \
        libkf6i18n-dev \
        libkf6statusnotifieritem-dev \
        libkf6iconthemes-dev \
        libkf6notifications-dev \
        libkf6unitconversion-dev \
        libkf6windowsystem-dev \
        kirigami-addons-dev \
        libkirigami-dev \
        libplasma-dev \
        libqalculate-dev \
        libqt6svg6-dev \
        libxkbcommon-dev \
        qt6-multimedia-dev \
        locales \
        ninja-build \
        openbox \
        pkg-config \
        python3 \
        python3-pyatspi \
        qml6-module-org-kde-kirigami \
        qml6-module-org-kde-kirigamiaddons-statefulapp \
        qml6-module-qtquick \
        qml6-module-qtquick-controls \
        qml6-module-qtquick-layouts \
        qml6-module-qtquick-templates \
        qml6-module-qtquick-window \
        qt6-base-dev \
        qt6-declarative-dev \
        wmctrl \
        x11-utils \
        xdotool \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

RUN sed -i 's/^# de_DE.UTF-8/de_DE.UTF-8/' /etc/locale.gen \
    && locale-gen de_DE.UTF-8

COPY kalk-affected /src/kalk-affected
COPY kalk-fixed /src/kalk-fixed
COPY elisa-affected /src/elisa-affected
COPY elisa-fixed /src/elisa-fixed

RUN for pair in kalk-affected kalk-fixed elisa-affected elisa-fixed; do \
        cmake -S "/src/$pair" -B "/src/$pair/build" -G Ninja \
            -DCMAKE_BUILD_TYPE=Release \
            -DCMAKE_INSTALL_PREFIX="/opt/reproit/$pair" \
            -DBUILD_TESTING=OFF > "/tmp/$pair.log" 2>&1 \
        && ninja -C "/src/$pair/build" install >> "/tmp/$pair.log" 2>&1 \
        || { echo "FAILED $pair"; tail -30 "/tmp/$pair.log"; exit 1; }; \
    done \
    && rm -rf /src


RUN dpkg-query -W -f='${binary:Package}\t${Version}\t${Architecture}\n' \
        | sort > /opt/reproit/build-packages.tsv \
    && { \
        cc --version; \
        qmake6 -v; \
        cmake --version; \
        python3 --version; \
    } > /opt/reproit/toolchain.txt 2>&1

COPY probe.py /opt/reproit/probe.py
COPY atspi_helpers.py /opt/reproit/atspi_helpers.py

ENTRYPOINT ["dbus-run-session", "--", "python3", "/opt/reproit/probe.py"]
