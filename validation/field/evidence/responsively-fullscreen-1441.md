# electron-linux field campaign: responsively-fullscreen-1441

- Repository: https://github.com/responsively-org/responsively-app
- Issue: https://github.com/responsively-org/responsively-app/issues/1441
- Affected revision: 48a6013c06c714fbacdfba9c3263f7622e672f75
- Fixed revision: bf1993b5677523a428d7ed190d3a8580f17c523f
- Expected identity: webview-keydown:f-suppressed-in-focused-text-input
- Minimized action: focus the previewed page text input and deliver one f key through the webview
- Neighboring legal behavior: a different printable key in the same focused guest text input still reaches the page

| Phase | Role | Run | Exit | ms | stdout sha256 | stderr sha256 |
|---|---|---|---|---|---|---|
| prepare | - | - | 0 | 877 | a2348617b920 | e8ef39fe6b55 |
| reset | affected | 1 | 0 | 33 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 1 | 0 | 59078 | 72e67feaadb0 | 9ef77caf8c8d |
| launch | affected | 1 | 0 | 250 | 8ddf80ec2af0 | e3b0c44298fc |
| readiness | affected | 1 | 0 | 9029 | d08ba2fbd942 | 7b935dd949ee |
| trigger | affected | 1 | 0 | 2849 | b767d1ff3376 | e3b0c44298fc |
| observe | affected | 1 | 0 | 2827 | 705c8bd02566 | e3b0c44298fc |
| reset | affected | 2 | 0 | 154 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 2 | 0 | 60537 | 72e67feaadb0 | 9ef77caf8c8d |
| launch | affected | 2 | 0 | 286 | d1e2bc44062c | e3b0c44298fc |
| readiness | affected | 2 | 0 | 8893 | d08ba2fbd942 | 7b935dd949ee |
| trigger | affected | 2 | 0 | 2862 | b767d1ff3376 | e3b0c44298fc |
| observe | affected | 2 | 0 | 2830 | fb3b29e11d1b | e3b0c44298fc |
| reset | affected | 3 | 0 | 126 | e3b0c44298fc | e3b0c44298fc |
| build | affected | 3 | 0 | 58053 | 72e67feaadb0 | 9ef77caf8c8d |
| launch | affected | 3 | 0 | 258 | 803b828f1687 | e3b0c44298fc |
| readiness | affected | 3 | 0 | 8957 | d08ba2fbd942 | 7b935dd949ee |
| trigger | affected | 3 | 0 | 2866 | b767d1ff3376 | e3b0c44298fc |
| observe | affected | 3 | 0 | 2862 | 11d276d18d9c | e3b0c44298fc |
| reset | fixed | 1 | 0 | 148 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 1 | 0 | 57744 | 509da3177c4b | 9ef77caf8c8d |
| launch | fixed | 1 | 0 | 241 | 4de492d7b90a | e3b0c44298fc |
| readiness | fixed | 1 | 0 | 8973 | d08ba2fbd942 | 7b935dd949ee |
| trigger | fixed | 1 | 0 | 2856 | b767d1ff3376 | e3b0c44298fc |
| observe | fixed | 1 | 0 | 800 | 3a53bc8b0124 | e3b0c44298fc |
| reset | fixed | 2 | 0 | 160 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 2 | 0 | 58531 | 509da3177c4b | 9ef77caf8c8d |
| launch | fixed | 2 | 0 | 249 | 26ec605cd8fb | e3b0c44298fc |
| readiness | fixed | 2 | 0 | 8946 | d08ba2fbd942 | 7b935dd949ee |
| trigger | fixed | 2 | 0 | 2863 | b767d1ff3376 | e3b0c44298fc |
| observe | fixed | 2 | 0 | 846 | 80e641af6069 | e3b0c44298fc |
| reset | fixed | 3 | 0 | 155 | e3b0c44298fc | e3b0c44298fc |
| build | fixed | 3 | 0 | 58568 | 509da3177c4b | 9ef77caf8c8d |
| launch | fixed | 3 | 0 | 247 | 0d34bbed1e3e | e3b0c44298fc |
| readiness | fixed | 3 | 0 | 9062 | d08ba2fbd942 | 7b935dd949ee |
| trigger | fixed | 3 | 0 | 2872 | b767d1ff3376 | e3b0c44298fc |
| observe | fixed | 3 | 0 | 806 | 656c01ce029d | e3b0c44298fc |
| minimize | - | - | 0 | 73517 | 0e194d47f962 | 9ef77caf8c8d |
| control | - | - | 0 | 2892 | aff976c36ca4 | e3b0c44298fc |
| cleanup | - | - | 0 | 501 | d07e740d159f | e3b0c44298fc |
| retain | - | - | 0 | 367 | 0c14560dea94 | e3b0c44298fc |

