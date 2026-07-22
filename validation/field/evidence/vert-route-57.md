# VERT issue 57 field review

The public report states that directly opening `/about` displays the home page. The affected
revision was the commit immediately before the navigation fix. The fixed revision includes both
the route-path change and the follow-up static-host correction linked to the issue discussion.

The application was built with its locked Bun dependencies and served with the same static-file
fallback and cross-origin headers declared by its nginx configuration. Each attempt used a new
Chromium context. On all three affected attempts, `/about` retained that URL but rendered the home
heading instead of `Why VERT?`. On all three fixed attempts, the About content rendered without a
browser exception. The root route was also checked as the neighboring legal behavior.

The minimized reproduction is only a clean direct navigation to `/about`. Review classified the
stable identity `route:/about:missing-text:Why VERT?` as the target application bug described by
the public issue.

Reproduction harness: `validation/field/probe-browser.mjs` and
`validation/field/serve-static-fallback.mjs`.
