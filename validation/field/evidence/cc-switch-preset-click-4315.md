# tauri-linux field campaign: cc-switch-preset-click-4315

- Repository: https://github.com/farion1231/cc-switch
- Issue: https://github.com/farion1231/cc-switch/issues/4302
- Affected revision: caa912e3a39c60330fad641b295ae8b13cdea586
- Fixed revision: 81d6002ace328cf74c9b63e32b15279a7c445812
- Expected identity: preset-search:result-not-selected-by-pointer
- Minimized action: type kimi into the provider preset search and press the Kimi result once
- Neighboring legal behavior: the same pointer press on a preset reached without the search still selects it

| Phase | Role | Run | Exit | ms | stdout sha256 | stderr sha256 |
|---|---|---|---|---|---|---|
| prepare | - | - | 0 | 2400 | c7dd10b385aa | ea62909ad186 |
| reset | affected | 1 | 0 | 32 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 1 | 0 | 104716 | e97a5bff896c | bd436b5e1ddc |
| launch | affected | 1 | 0 | 382 | de54304ac708 | e3b0c44298fc |
| readiness | affected | 1 | 0 | 13894 | 7a6b39c44470 | e049189b24c3 |
| trigger | affected | 1 | 0 | 3638 | b8225ab84532 | e3b0c44298fc |
| observe | affected | 1 | 0 | 600 | a502ff040cf0 | e3b0c44298fc |
| reset | affected | 2 | 0 | 155 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 2 | 0 | 100350 | 98e59f8b5b8b | aa9e19e539e6 |
| launch | affected | 2 | 0 | 268 | 12c9e3060826 | e3b0c44298fc |
| readiness | affected | 2 | 0 | 13710 | 7a6b39c44470 | e049189b24c3 |
| trigger | affected | 2 | 0 | 3604 | b8225ab84532 | e3b0c44298fc |
| observe | affected | 2 | 0 | 602 | b8b31180295e | e3b0c44298fc |
| reset | affected | 3 | 0 | 148 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 3 | 0 | 99210 | cc6ffe1f98df | aa80737237db |
| launch | affected | 3 | 0 | 264 | 20697daa56fc | e3b0c44298fc |
| readiness | affected | 3 | 0 | 13711 | 7a6b39c44470 | e049189b24c3 |
| trigger | affected | 3 | 0 | 3581 | b8225ab84532 | e3b0c44298fc |
| observe | affected | 3 | 0 | 703 | 245d21f95931 | e3b0c44298fc |
| reset | fixed | 1 | 0 | 160 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 1 | 0 | 112266 | b6c3f43c62a4 | f91f6e01cefb |
| launch | fixed | 1 | 0 | 283 | 850b1195e164 | e3b0c44298fc |
| readiness | fixed | 1 | 0 | 13543 | 7a6b39c44470 | e049189b24c3 |
| trigger | fixed | 1 | 0 | 3583 | b8225ab84532 | e3b0c44298fc |
| observe | fixed | 1 | 0 | 589 | 71ad9a5359bd | e3b0c44298fc |
| reset | fixed | 2 | 0 | 153 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 2 | 0 | 98596 | 76a71d002185 | a12749d2c9cd |
| launch | fixed | 2 | 0 | 322 | e408077057a0 | e3b0c44298fc |
| readiness | fixed | 2 | 0 | 13658 | 7a6b39c44470 | e049189b24c3 |
| trigger | fixed | 2 | 0 | 3708 | b8225ab84532 | e3b0c44298fc |
| observe | fixed | 2 | 0 | 617 | 1b564110a25b | e3b0c44298fc |
| reset | fixed | 3 | 0 | 179 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 3 | 0 | 115434 | a054ff87d965 | 2338ce0a36e5 |
| launch | fixed | 3 | 0 | 240 | eb3b65194f83 | e3b0c44298fc |
| readiness | fixed | 3 | 0 | 13590 | 7a6b39c44470 | e049189b24c3 |
| trigger | fixed | 3 | 0 | 3526 | b8225ab84532 | e3b0c44298fc |
| observe | fixed | 3 | 0 | 633 | e32c19dd95e7 | e3b0c44298fc |
| minimize | - | - | 0 | 133487 | a564541fd4cc | 81f04937b35b |
| control | - | - | 0 | 2964 | 83d46154f8f5 | e3b0c44298fc |
| cleanup | - | - | 0 | 534 | d07e740d159f | e3b0c44298fc |
| retain | - | - | 0 | 606 | 12e2abf8b3a6 | e3b0c44298fc |

