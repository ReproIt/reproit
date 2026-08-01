# kiwix-apple iPad sidebar with no ZIM archive, issue 1607

Companion prose for `kiwix-ipad-no-zim-sidebar-1607.json`, the second of the two
swiftui-ios field campaign applications. It is an independent repository, an
independent upstream project, and a different half of the adapter from the
dimeApp campaign: scene and split-view composition rather than control state.

## The pair

`6f4b760c` is the merge commit of pull request 1608 and `f3686fe4` is its first
parent. The diff touches three files, all SwiftUI: `App/RootView_iOS.swift`,
`App/SplitViewForiPad.swift`, and `SwiftUI/Model/DefaultKeys.swift`. The load
bearing change is the deletion of `updateColumnVisibility`, which the affected
revision calls from `.task` and from `.onChange(of: navigation.currentItem)`:

    private func updateColumnVisibility() {
        if hasZimFiles == true, navigation.currentItem != .loading {
            columnVisibility = Defaults[.ipadSplitViewVisibility]
        } else if hasZimFiles == false {
            columnVisibility = .detailOnly
        }
    }

With no ZIM archive imported, `hasZimFiles` is false, so every change of
`navigation.currentItem` slams the split view back to `.detailOnly`.

## Materialising the project

kiwix-apple commits no xcodeproj. Both revisions are generated the way the
Brewfile documents: fetch `libkiwix_xcframework-14.2.1-2.tar.gz`, copy
`Support/CoreKiwix.modulemap` into the three xcframework slices, run
`python3 localizations.py generate`, then run XcodeGen 2.46.0 over
`project.yml`. Both revisions pin the same libkiwix tarball. No application
source is edited at either revision.

## The minimized trigger

On an iPad simulator with no ZIM archive present, tap Show Sidebar, then tap
New Tab. Nothing else.

New Tab is the one control on the screen that changes
`navigation.currentItem`. Selecting a sidebar entry such as Bookmarks changes
only the `List` selection, which is the neighboring legal action below.

## The observation

The observable is whether the split view still exposes its Hide Sidebar control
after the tab action, which is a direct read of the column visibility state
rather than a pixel or layout comparison.

    affected   launch hidden  after toggle shown  after New Tab hidden
    fixed      launch hidden  after toggle shown  after New Tab shown

Three affected runs land on
`ipad-sidebar-collapses-on-selection-without-zim-archive` and three fixed runs
reach the same observation point and report nothing. Both revisions launch with
the sidebar hidden, so the launch state is not what separates them.

## Neighboring legal behavior

On the same affected build, opening the sidebar and then selecting Bookmarks
leaves the sidebar open, because that path does not touch
`navigation.currentItem`. The control run records exactly that, so the
observation is not merely reporting that the sidebar can be hidden.

An earlier formulation of this control closed the sidebar explicitly and then
tried the same New Tab action. It was replaced rather than reported, because
with the sidebar closed the New Tab control is not addressable at all, so that
run never reached an observation and proved nothing either way.

## Reset

Every run terminates, uninstalls, and reinstalls the application, so each run
starts with an empty library, no downloads, and the default split view state.
