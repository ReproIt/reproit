# Slidev issue 1637, WebKit review

The report says next and previous navigation are broken when `routerMode: hash` is
enabled. Pull request 1640 links the fix.

The affected checkout is the pull request base, and the fixed checkout is its merge
commit. With the single `routerMode: hash` configuration change, three clean WebKit
launches stayed on `#/1` after one ArrowRight press. Three clean fixed launches reached
`#/2` and rendered the second slide. Directly opening `#/2` passed at both revisions,
which isolates navigation from route rendering.

The minimized trigger is one ArrowRight press from `#/1`. Review confirmed
`navigation:#/1:stuck-on-arrow-right` as the reported bug. JavaScript heap measurement
is unavailable through Playwright WebKit and is recorded as such.

Harness: `validation/field/probe-browser.mjs`.
