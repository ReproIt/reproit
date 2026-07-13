---
name: reproit-screenshots
description: >-
  Use when generating store or marketing screenshots with reproit: authoring a
  "tour" (a journey whose `do: shoot:<name>` steps name the shots) and running
  `reproit screenshots` across locales and devices. Trigger on "app store
  screenshots", "marketing screenshots", "screenshot every screen", "fastlane
  screenshots", "localized / RTL screenshots", or editing a screenshot tour.
---

# Authoring reproit screenshot tours

A **tour** is an ordinary reproit journey whose `do: shoot:<name>` steps name the
screenshots to capture. The same file is two things at once:

- `reproit journey <tour>` runs the steps to verify behavior; `shoot:` is **inert**
  (navigate-only, no pictures, no overhead).
- `reproit screenshots <tour>` runs the same steps in **capture mode**: each
  `shoot:<name>` writes `<name>.png`, fanned across locales and devices.

Because the state signature is locale-invariant, **one tour covers every locale**
with no per-locale selectors. Start from `examples/journeys/marketing.yaml`.

## The craft: what makes screenshots *ideal* (not just captured)

Most of the value is in HOW you author the tour. A capture tool that shoots the
wrong screen, mid-transition, with empty data, in one locale, is worse than none.

1. **Curate, do not dump.** Stores show ~3-10 screenshots. Pick the 3-8 *hero*
   screens that sell the app (the value moment, the key feature, the result), not
   every screen. Do NOT auto-shoot every step. Each `shoot:` is a deliberate
   marketing asset.
2. **Compose the screen before you shoot.** Navigate to a state that looks good:
   content loaded, the feature visible, a realistic selection made. Put the
   `shoot:` step *after* the taps/types that compose it. reproit auto-settles
   between actions, so never add sleeps.
3. **Seed real, attractive data.** Empty states and lorem ipsum kill store
   screenshots. Begin with `setup: login(demo)` and a `reset:` prelude that seeds
   realistic content (a few posts, a populated list, a non-zero balance). Store
   demo creds with `reproit auth add`; reference accounts under `auth:`.
4. **Name shots to their store slot / meaning.** The name becomes the filename
   AND the per-locale matching key for the verify gate. Use `home`, `search`,
   `detail`, `checkout`, not `screen1`. Keep names in `[A-Za-z0-9_/-]`.
5. **Cover locales, including the hard ones.** Pass `--locale en,de,ar,ja` (or
   `screenshots.locales`). German (long words) and Arabic/Hebrew (RTL) are where
   layouts break; the locale-invariant tour reuses the SAME steps for all of
   them, so you author once.
6. **Target the device classes stores require.** iOS: a 6.7"/6.9" iPhone and a
   13" iPad. Android: phone + 7" + 10" tablet. List them in
   `screenshots.devices` / `--device`. On the iOS simulator reproit sets a clean
   status bar (9:41, full battery) automatically; keep it for store shots.
7. **Verify completeness.** The gate cross-checks that every locale of a
   platform/device produced the SAME set of shots, so a screen that drifted or
   was skipped in one locale fails loudly instead of shipping a gap. Leave
   `verifySignature: true`.

## Run it

```sh
reproit screenshots marketing \
  --locale en,de,ar,ja \
  --device "iPhone 16 Pro Max,iPad Pro 13" \
  --out screenshots
reproit journey marketing        # dry-run the navigation, no pictures
```

Config (`reproit.yaml`):

```yaml
screenshots:
  tour: marketing
  out: screenshots
  locales: [en, de, ar, ja]
  devices: ["iPhone 16 Pro Max", "iPad Pro 13"]
  verifySignature: true
```

## Output layout and fastlane

Shots land journey-led, collapsing axes that do not vary:
`screenshots/<journey>[/<platform>][/<locale>][/<device>]/<name>.png`. For the
exact structure `fastlane deliver` (iOS) / `supply` (Android) expect, set a
`--path-template` / `screenshots.pathTemplate` with the placeholders
`{journey} {platform} {locale} {device}`, e.g. `"{locale}/{device}"`.

## Common mistakes

- Shooting every screen (noise) instead of curating hero screens.
- Capturing empty/seed/lorem data, or mid-transition (shoot before the screen
  composed).
- Forgetting RTL/long-locale coverage, where the layout actually breaks.
- Writing per-locale tours: unnecessary, the signature is locale-invariant, one
  tour serves all.
- Using `reproit check` and expecting pictures: that is verify mode (inert).
  Pictures only come from `reproit screenshots`.
