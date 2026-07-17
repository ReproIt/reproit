# Google Play Data safety disclosure

If you ship an app containing the ReproIt Android SDK, Google Play requires you to declare the data
the SDK collects in your app's Data safety form. You are responsible for your own declaration; this
page states exactly what this SDK collects so you can fill the form accurately. The authoritative
technical contract is [docs/data-handling.md](../../docs/data-handling.md).

## What the SDK collects

| Play form category                                                                   | Collected | Details                                                                                                                                                                                                                             |
| ------------------------------------------------------------------------------------ | --------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| App activity: App interactions                                                       | Yes       | Structural screen signatures (hashes of UI shape, text stripped) and the action path between screens. Visible control labels are included by default and can be disabled with `redactLabels`.                                       |
| App info and performance: Crash logs                                                 | Yes       | Crash signature, normalized error message, stack trace, and the structural path that led to the crash.                                                                                                                              |
| App info and performance: Diagnostics                                                | Yes       | Platform name, Android OS version string, locale, timezone.                                                                                                                                                                         |
| Device or other IDs                                                                  | No        | The SDK reads no ANDROID_ID, IMEI, serial, advertising id, or any other device identifier.                                                                                                                                          |
| Personal info (name, email, address, ...)                                            | No        | Never collected by the SDK. If your app supplies a user id via the SDK API, only a truncated SHA-256 hash of it is transmitted, as a grouping key; the raw id never leaves the device. Declare "User IDs" only if you use that API. |
| Financial info, location, contacts, photos, messages, audio, files, calendar, health | No        | Never collected.                                                                                                                                                                                                                    |

Text your users type is never transmitted. On an error, input fields are reduced to derived features
only (length, byte count, character classes, scripts); password and hidden fields are never read at
all.

## Form answers

- **Is data encrypted in transit?** Yes, provided you configure an `https://` endpoint (the SDK
  posts to the endpoint you supply and hardcodes none; always use HTTPS in production).
- **Is data shared with third parties?** No. The SDK transmits only to the endpoint you configure.
- **Can users request data deletion?** Yes, server-side: telemetry is deleted automatically at the
  end of the workspace retention window, and deleting a workspace deletes its database and stored
  evidence. The SDK itself persists nothing on the device (no files, no SharedPreferences; the event
  queue is in-memory only).
- **Purpose:** App functionality (crash reproduction and debugging).
- **Is collection optional or required?** Determined by your integration; the SDK only runs where
  you attach it.
