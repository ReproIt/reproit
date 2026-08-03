# Platypus issue 230 macOS Accessibility evidence

The exact affected and fixed commits were built from
`https://github.com/sveinbjornt/Platypus` with Xcode 26.2 on macOS 26.1 arm64.
The affected setup build completed in 18 seconds. Both revisions used the
`Development` configuration, isolated `SYMROOT`, `OBJROOT`, and
`CONFIGURATION_TEMP_DIR` directories, ad hoc signing, and
`ENABLE_HARDENED_RUNTIME=NO`. No application source was patched.

The minimized trigger launches the ordinary main window and reads `AXHelp`
from the `AXCheckBox` titled `Send notifications`. All three affected runs
returned the Dock help text belonging to `Run in background`. All three fixed
runs returned `Check this if your app sends system notifications`.

The neighboring control reads `AXHelp` from `Run in background` in the same
snapshot. Its Dock help text is correct and unchanged at both revisions. This
keeps the identity tied to the checkbox owner, rather than flagging that help
text wherever it appears.

Each replay used a separately copied and re-signed application with a unique
bundle identifier and a unique `CFFIXED_USER_HOME`. HTTP(S) and ALL proxy
variables pointed at closed loopback port 9. `lsof` found no established
external connection in any run. The harness terminated only the process whose
executable path was inside that run's application copy. Fourteen campaign
network checks across both applications and their controls found zero external
connections.

The retained structured record contains the three affected reproductions,
three fixed controls, exact help values, and replay measurements. Manual review
confirmed that the reported identity is issue 230 and not the neighboring legal
behavior.
