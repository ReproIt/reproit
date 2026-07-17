// ReproIt macOS SwiftUI smoke fixture: the desktop counterpart of the iOS
// SwiftUI fixture, the target the macOS AX runner (runners/macos-ax.swift)
// drives to prove the desktop backend handles a REAL SwiftUI app (not just
// AppKit). The site marquee claims SwiftUI; the desktop AX runner reaches the
// system accessibility tree, and SwiftUI publishes to that same API, so this
// fixture is the proof.
//
// One window: a headline Text, a Button, and a detail Text that is present only
// while @State `revealed` is true. Every control carries an explicit
// .accessibilityIdentifier (surfaced as AXIdentifier), and the button's title
// is a stable label the runner's walk taps by (tap:<label>). Toggling
// structurally adds/removes the identified detail Text, so a tap provably moves
// the app to a new canonical signature (EXPLORE:EDGE).
//
// Compiled directly with swiftc against the macosx SDK by build.sh
// (-parse-as-library so @main is honoured), no Xcode project, no signing.
import SwiftUI

struct ContentView: View {
  @State private var revealed = false

  var body: some View {
    VStack(spacing: 24) {
      Text("ReproIt SwiftUI Fixture")
        .font(.headline)
        .accessibilityIdentifier("fixture.title")

      Button(revealed ? "Hide detail" : "Reveal detail") {
        revealed.toggle()
      }
      .accessibilityIdentifier("fixture.toggle")

      // Present only while revealed: its presence/absence is the
      // structural state change the smoke asserts on.
      if revealed {
        Text("Detail revealed")
          .accessibilityIdentifier("fixture.detail")
      }
    }
    .padding(40)
    .frame(width: 360, height: 260)
  }
}

@main
struct FixtureApp: App {
  var body: some Scene {
    WindowGroup {
      ContentView()
    }
  }
}
