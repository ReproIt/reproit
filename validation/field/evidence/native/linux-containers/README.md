# Linux container exact native evidence

This directory retains the canonical `linux-atspi-gtk`, `linux-atspi-toolkits`,
and `tauri` native release gates for commit
`414c1a14c7d60e1127a7738834e995020366fed0`.

- Executor: native Linux x86_64 (Fedora host `strix`), Docker engine
  `x86_64/linux`
- Route: `ssh black@zgx-5a09.local`, then `ssh strix`
- Collector: `validation/release/run-linux-x86-remote.sh --gate linux-atspi-gtk
  --gate linux-atspi-toolkits --gate tauri`
- Source mode: exact, verified by archive digest, Git commit, host architecture,
  and Docker architecture
- Results: `linux-atspi-gtk.json`, `linux-atspi-toolkits.json`, `tauri.json`
- Captured logs: `linux-atspi-gtk.log`, `linux-atspi-toolkits.log`, `tauri.log`
- Validated summary: `validated-summary.json`
- Run metadata: `run-metadata.json`
- Log SHA-256:
  - `linux-atspi-gtk.log`:
    `79a02c0821fd6ebfd5b6e888773d8d9c7571457cce9c7dedc31602390f7c4460`
  - `linux-atspi-toolkits.log`:
    `2869c89d7f4495846fb964d9b88f272d4acad0e6698722762d878c5818cef58d`
  - `tauri.log`:
    `758539e91be026283fe8a464e00b98ceea84580297e6080dfbe1bd15e967a832`
- Validated summary SHA-256:
  `005bbff80bc55d5b1fc7726c844e4cf85110d9c28553d96146e4a82e79e5c9a2`
- Run metadata SHA-256:
  `9aa6afffa913b355ad62242d98a8a5911635b7f5a7712ce561fca9c080e9ca23`

Every required output marker passed with exit code zero. The GTK gate reached
both owned fixture processes on the AT-SPI application bus, which is the
condition the earlier emulated amd64 diagnostic could not reach; see
`validation/field/evidence/linux-atspi-amd64-local.md`. The toolkit gate
exercises Qt Widgets, Qt Quick/QML, and wxWidgets in one run. The collector
removed its owned containers, images, and remote run directory on exit.

Native fixture success does not promote any target. Linux GTK, Linux Qt
Quick/QML, Linux Qt Widgets, Linux wxWidgets, and Tauri Linux still need their
two-independent-application field campaigns and per-target corpus records.
