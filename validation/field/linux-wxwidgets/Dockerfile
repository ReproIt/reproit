FROM debian:bookworm@sha256:9344f8b8992482f80cba753f323adeaf17690076c095ccff6cc9536be98185dc

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        at-spi2-core \
        build-essential \
        cmake \
        dbus-x11 \
        gettext \
        libatk-adaptor \
        libatspi2.0-0 \
        libwxgtk3.2-dev \
        maxima \
        ninja-build \
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
    && cmake --build /src/wxmaxima-affected/build --parallel 4 \
    && install -D /src/wxmaxima-affected/build/src/wxmaxima \
        /opt/reproit/wxmaxima-affected \
    && cmake -S /src/wxmaxima-fixed -B /src/wxmaxima-fixed/build \
        -G Ninja -DCMAKE_BUILD_TYPE=Release \
    && cmake --build /src/wxmaxima-fixed/build --parallel 4 \
    && install -D /src/wxmaxima-fixed/build/src/wxmaxima \
        /opt/reproit/wxmaxima-fixed \
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
