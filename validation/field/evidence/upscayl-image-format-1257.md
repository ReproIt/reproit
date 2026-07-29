# electron-linux field campaign: upscayl-image-format-1257

- Repository: https://github.com/upscayl/upscayl
- Issue: https://github.com/upscayl/upscayl/issues/1225
- Affected revision: 86c144b1e3311c26c241c20d8b0a625462542cad
- Fixed revision: d736736e3cb9a2cf6ca8b06c0f95034abeb812e3
- Expected identity: settings:save-image-as-not-restored-after-reload
- Minimized action: click the JPG export-format button in Settings, then reload the renderer
- Neighboring legal behavior: another setting stored through the same JSON-encoded mechanism still survives a reload

| Phase | Role | Run | Exit | ms | stdout sha256 | stderr sha256 |
|---|---|---|---|---|---|---|
| prepare | - | - | 0 | 820 | eeb2ed6f12b7 | 67b1aa80ac49 |
| reset | affected | 1 | 0 | 28 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 1 | 0 | 93460 | 1da6ef3abea2 | 235184d5cc17 |
| launch | affected | 1 | 0 | 238 | 5e6c5a8b6a21 | e3b0c44298fc |
| readiness | affected | 1 | 0 | 12049 | fb8c73ba2079 | 7b935dd949ee |
| trigger | affected | 1 | 0 | 13958 | cfabd42c427e | e3b0c44298fc |
| observe | affected | 1 | 0 | 828 | 994e8d6e9f49 | e3b0c44298fc |
| reset | affected | 2 | 0 | 135 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 2 | 0 | 92802 | 1da6ef3abea2 | 235184d5cc17 |
| launch | affected | 2 | 0 | 252 | d2ec8275c364 | e3b0c44298fc |
| readiness | affected | 2 | 0 | 12031 | fb8c73ba2079 | 7b935dd949ee |
| trigger | affected | 2 | 0 | 14032 | cfabd42c427e | e3b0c44298fc |
| observe | affected | 2 | 0 | 796 | 592f29d27a95 | e3b0c44298fc |
| reset | affected | 3 | 0 | 137 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 3 | 0 | 91881 | 1da6ef3abea2 | 235184d5cc17 |
| launch | affected | 3 | 0 | 236 | e1e921e20acc | e3b0c44298fc |
| readiness | affected | 3 | 0 | 12180 | fb8c73ba2079 | 7b935dd949ee |
| trigger | affected | 3 | 0 | 13990 | cfabd42c427e | e3b0c44298fc |
| observe | affected | 3 | 0 | 883 | 84848e95b542 | e3b0c44298fc |
| reset | fixed | 1 | 0 | 146 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 1 | 0 | 99534 | ac1e7cbfa0e6 | 6522deedf968 |
| launch | fixed | 1 | 0 | 293 | 34058ade6cf8 | e3b0c44298fc |
| readiness | fixed | 1 | 0 | 12000 | fb8c73ba2079 | 7b935dd949ee |
| trigger | fixed | 1 | 0 | 13956 | a747dcc7eb3f | e3b0c44298fc |
| observe | fixed | 1 | 0 | 793 | 8e8f2f8edc85 | e3b0c44298fc |
| reset | fixed | 2 | 0 | 156 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 2 | 0 | 93822 | ac1e7cbfa0e6 | 235184d5cc17 |
| launch | fixed | 2 | 0 | 260 | 15a96066dc0d | e3b0c44298fc |
| readiness | fixed | 2 | 0 | 12121 | fb8c73ba2079 | 7b935dd949ee |
| trigger | fixed | 2 | 0 | 13983 | a747dcc7eb3f | e3b0c44298fc |
| observe | fixed | 2 | 0 | 802 | 3e3e7ca07c92 | e3b0c44298fc |
| reset | fixed | 3 | 0 | 121 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 3 | 0 | 92784 | ac1e7cbfa0e6 | 235184d5cc17 |
| launch | fixed | 3 | 0 | 254 | 67042a2a92e4 | e3b0c44298fc |
| readiness | fixed | 3 | 0 | 12000 | fb8c73ba2079 | 7b935dd949ee |
| trigger | fixed | 3 | 0 | 13991 | a747dcc7eb3f | e3b0c44298fc |
| observe | fixed | 3 | 0 | 815 | f81b5f24fd74 | e3b0c44298fc |
| minimize | - | - | 0 | 123017 | 8a04d1143f10 | 6522deedf968 |
| control | - | - | 0 | 8973 | 3d704b7c6283 | e3b0c44298fc |
| cleanup | - | - | 0 | 587 | d07e740d159f | e3b0c44298fc |
| retain | - | - | 0 | 406 | 39d1616bf1c7 | e3b0c44298fc |

