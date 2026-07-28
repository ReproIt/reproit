# VERT issue 57, Firefox review

The report says that directly opening `/about` renders the home page. The affected
revision precedes the linked route fix, and the fixed revision includes the static-host
follow-up.

The locked application was built and served with its declared static-file fallback.
Three clean Firefox launches at the affected revision retained `/about` but showed the
home heading. Three clean launches at the fixed revision showed `Why VERT?`. The root
route passed as neighboring legal behavior.

The minimized trigger is a direct navigation to `/about`. Review confirmed
`route:/about:missing-text:Why VERT?` as the reported application bug. Playwright does
not expose JavaScript heap measurement for Firefox, so memory is recorded as
unavailable rather than zero.

Harness: `validation/field/probe-browser.mjs` and
`validation/field/serve-static-fallback.mjs`.
