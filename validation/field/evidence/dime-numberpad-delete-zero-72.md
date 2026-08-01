# dimeApp number pad delete, issue 72

Companion prose for `dime-numberpad-delete-zero-72.json`, the first of the two
swiftui-ios field campaign applications.

## The pair

`c6d4fc0a` is the first parent of `0463cb8c`, the merge commit of pull request 77.
The whole diff between them is one line in
`app/dime/Components/Transactions/NumberPad.swift`:

    -       .disabled(price == 0)
    +       .disabled(price == 0 && !isEditingDecimal)

That line is the delete key inside `struct NumberPadTextView: View`, so the file
the fix touches is itself a SwiftUI view and the authority is the authored
contract of the pull request rather than an inference from the symptom.

## The surface, and why it is this one

`NumberPadTextView` has three call sites: the transaction view, the new budget
view, and `SettingsNumberEntryView`. The settings screen was chosen because it
hosts a fully live pad with its own `price`, `isEditingDecimal` and
`decimalValuesAssigned` state and needs no transaction, no budget, and no
account. It is reached with three taps from the tab bar.

Two preconditions are setup rather than trigger. The welcome sheet is dismissed
with Get Started, and one suggested expense category is added, because the
first-run category sheet refuses to close while the category list is empty and
it covers the tab bar. The delete key is also only rendered when
`numberEntryType == 2`, so the run selects Type 2 on the same settings screen
before observing anything.

## The minimized trigger

Tap the decimal key once, then tap the delete key once. Nothing else.

After the decimal tap `price` is still 0 and `isEditingDecimal` is true, so the
amount label reads `$0.`. On the affected revision the delete key's disabled
condition is `price == 0`, which is true, so the key is inert and the amount
does not move. On the fixed revision the condition also requires
`!isEditingDecimal`, so the key is live and `deleteLastDigit` clears the pending
decimal back to `$0`.

## The observation

The observable is a pair the driver reads directly: the `enabled` attribute of
the `delete.left.fill` button and the text of the amount label. It is a hard
state oracle, not a layout heuristic and not a pixel comparison.

    affected   amount $0.  delete enabled false  amount after delete $0.
    fixed      amount $0.  delete enabled true   amount after delete $0

Three affected runs land on
`delete-key-inert-while-decimal-pending-at-zero` and three fixed runs reach the
same observation point and report nothing.

## Neighboring legal behavior

On the same affected build and the same pad, typing `5` and then tapping delete
is legal: the key is enabled and the amount goes from `$5` to `$0`. The control
run records exactly that, so the observation is not merely reporting that a
delete key exists on the screen.

## Reset

Every run terminates, uninstalls, and reinstalls the application, so each run
starts from a first-launch container with no categories, no transactions, and
the default number entry method.

## Build

Both revisions build for the simulator with ad-hoc signing. An unsigned build
installs and dies on launch with SIGTRAP because `DataController` installs
`NSPersistentCloudKitContainerOptions` unconditionally and calls `fatalError`
when the store fails to load; keeping the project entitlements and signing
ad-hoc with the team blanked launches normally. No application source was
changed at either revision.
