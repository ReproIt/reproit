# GreenStash currency-picker rotation field evidence

GreenStash issue 213 was replayed at affected revision
`eeb3bc077f796ce45cae66c45a00c82def9ee599` and fixed revision
`78a1cf4aaa5a673e50bc54f9bbed66d4e6514200`.

Every observation used a new API 36 x86_64 AVD, emulator `-wipe-data`,
snapshots disabled, Docker network mode `none`, and a fresh Appium 3.5.2
UiAutomator2 8.0.0 session. The requested capabilities and every session ID
are retained in the structured record.

All three affected runs lost the open picker, Japanese Yen search text, and
selected currency after rotation, returning to the default currency. All three
fixed controls retained the open picker, search text, and selected currency.
Rotating the untouched welcome screen kept the default currency visible on both
revisions, confirming the neighboring legal behavior.

One fixed run encountered an explicit `adb device offline` transport failure
during initial Appium session creation. The bounded infrastructure policy
discarded that AVD and retried on a newly reset AVD. The successful run retains
the attempt count and reason.

The APK digests, source hashes, screenshot hashes, reset evidence, and exact
CLI commit are recorded in the JSON evidence. Representative affected and
fixed screenshots were manually reviewed and confirm the structured result.
