# Slidev issue 2297 field review

The public report states that Slidev shortcuts fire while a user types in Monaco on Chromium. The
affected revision is the issue's reported base. The fixed revision is the merged commit that adds
an explicit shortcut lock while Monaco owns focus.

The repository was installed from its frozen pnpm lockfile and its workspace packages were built
at each revision. Each attempt opened the bundled starter at slide 15 in a new Chromium context,
focused the visible Monaco editor, verified the Chromium active element was
`DIV.native-edit-context`, and pressed Space once. All three affected attempts incorrectly moved
to slide 16. All three fixed attempts remained on slide 15 without a browser exception.

As the neighboring legal behavior, the same Space key was pressed with `BODY` focused at the fixed
revision. It advanced to slide 16 on all three attempts, proving that the fix did not disable the
valid presentation shortcut. The minimized reproduction is one focus action and one key press.
Review classified `navigation:/15->/16:on-space-in-monaco` as the target application bug described
by the public issue.

Reproduction harness: `validation/field/probe-browser.mjs`.
