# NextPlayer permission-loop field evidence

NextPlayer issue 1820 was replayed at affected revision
`b00807bc0ba28b41365c5f4e41e0af2062e7715e` and fixed revision
`b2875cc4d4e866912c04c26aff8b6fbff9e0de57`.

Every observation used a new API 36 x86_64 AVD, emulator `-wipe-data`,
snapshots disabled, Docker network mode `none`, and a fresh Appium 3.5.2
UiAutomator2 8.0.0 session. The requested capabilities and every session ID
are retained in the structured record.

All three affected runs exposed the loading indicator with media permission
still denied and no reachable permission prompt. All three fixed controls
accepted the system permission prompt through Appium and reached the empty
media view. Pregranting media permission reached the empty media view on both
revisions, confirming the neighboring legal behavior.

The APK digests, source hashes, screenshot hashes, reset evidence, and exact
CLI commit are recorded in the JSON evidence. Representative affected and
fixed screenshots were manually reviewed and confirm the structured result.
