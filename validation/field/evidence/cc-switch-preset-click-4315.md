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
| prepare | - | - | 0 | 2023 | c7dd10b385aa | f6ce54ab9bdd |
| reset | affected | 1 | 0 | 60 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 1 | 0 | 109238 | d070b031fa2c | cebc4ad55a1a |
| launch | affected | 1 | 0 | 239 | c16a806a7c08 | e3b0c44298fc |
| readiness | affected | 1 | 0 | 13624 | 7a6b39c44470 | 33339274722d |
| trigger | affected | 1 | 0 | 3616 | b8225ab84532 | e3b0c44298fc |
| observe | affected | 1 | 0 | 782 | 10e29f73a6cd | e3b0c44298fc |
| reset | affected | 2 | 0 | 164 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 2 | 0 | 124056 | 0a5eda5f1f94 | 4f06b9fce104 |
| launch | affected | 2 | 0 | 265 | c81b36c3e14e | e3b0c44298fc |
| readiness | affected | 2 | 0 | 16412 | 7a6b39c44470 | 33339274722d |
| trigger | affected | 2 | 0 | 3574 | b8225ab84532 | e3b0c44298fc |
| observe | affected | 2 | 0 | 667 | 1b68c40308cc | e3b0c44298fc |
| reset | affected | 3 | 0 | 224 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 3 | 0 | 105355 | 7717b560f470 | 6be0bb3b957e |
| launch | affected | 3 | 0 | 239 | a1c12d46f703 | e3b0c44298fc |
| readiness | affected | 3 | 0 | 13617 | 7a6b39c44470 | 33339274722d |
| trigger | affected | 3 | 0 | 3583 | b8225ab84532 | e3b0c44298fc |
| observe | affected | 3 | 0 | 654 | df324db116af | e3b0c44298fc |
| reset | fixed | 1 | 0 | 164 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 1 | 0 | 123570 | ee43526dd433 | 649f47bd28ca |
| launch | fixed | 1 | 0 | 248 | a93245c45ded | e3b0c44298fc |
| readiness | fixed | 1 | 0 | 13745 | 7a6b39c44470 | 33339274722d |
| trigger | fixed | 1 | 0 | 3546 | b8225ab84532 | e3b0c44298fc |
| observe | fixed | 1 | 0 | 608 | f6f332f03adc | e3b0c44298fc |
| reset | fixed | 2 | 0 | 157 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 2 | 0 | 103634 | 5f5fff3d1270 | 512cc5f852cf |
| launch | fixed | 2 | 0 | 259 | 6ef208eec7b5 | e3b0c44298fc |
| readiness | fixed | 2 | 0 | 13787 | 7a6b39c44470 | 33339274722d |
| trigger | fixed | 2 | 0 | 3527 | b8225ab84532 | e3b0c44298fc |
| observe | fixed | 2 | 0 | 617 | 400c47a1f16a | e3b0c44298fc |
| reset | fixed | 3 | 0 | 193 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 3 | 0 | 121723 | b97064577f48 | 16a4ce07c439 |
| launch | fixed | 3 | 0 | 278 | bd8735d6f342 | e3b0c44298fc |
| readiness | fixed | 3 | 0 | 13757 | 7a6b39c44470 | 33339274722d |
| trigger | fixed | 3 | 0 | 3591 | b8225ab84532 | e3b0c44298fc |
| observe | fixed | 3 | 0 | 613 | 61724b21971a | e3b0c44298fc |
| minimize | - | - | 0 | 136808 | 4402c8cbfd23 | 553506124afd |
| control | - | - | 0 | 3019 | 83d46154f8f5 | e3b0c44298fc |
| cleanup | - | - | 0 | 576 | d07e740d159f | e3b0c44298fc |
| retain | - | - | 0 | 523 | b8a968b7a990 | e3b0c44298fc |

