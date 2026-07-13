// ReproIt iOS SwiftUI smoke fixture: the SwiftUI counterpart of
// examples/ios-smoke-fixture (which is UIKit). The site marquee claims ReproIt
// drives SwiftUI apps; the UIKit fixture only proved the UIKit path, so this
// fixture proves the XCUITest backend drives a REAL SwiftUI app built with the
// modern @main App lifecycle, @State, and the declarative View tree.
//
// One screen: a headline Text, a Button, and a detail Text that is present only
// while @State `revealed` is true. Every control carries an explicit
// .accessibilityIdentifier, so the runner's structural snapshot is guaranteed a
// non-empty elements list with stable key:<id> selectors, and the button's tap
// structurally adds/removes the identified detail Text, so a tap provably
// produces a new canonical signature (EXPLORE:EDGE).
//
// Compiled directly with swiftc against the simulator SDK by build.sh
// (-parse-as-library so @main is honoured), no Xcode project, no signing,
// no storyboard. The whole app is this one file.
import SwiftUI
import Foundation

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
            // structural state change the smoke asserts on (a text node with a
            // stable id enters/leaves the canonical tree on each tap).
            if revealed {
                Text("Detail revealed")
                    .accessibilityIdentifier("fixture.detail")
            }
        }
        .padding()
    }
}

@main
struct FixtureApp: App {
    init() {
        ReproIt.start(ReproItConfig(appId: "swiftui-fixture"))
        URLSession.shared.dataTask(with: URL(string: "http://127.0.0.1:18766/bootstrap")!) { data, _, error in
            guard let path = ProcessInfo.processInfo.environment["REPROIT_NATIVE_RESULT_FILE"] else { return }
            let result = data ?? Data((error?.localizedDescription ?? "missing response").utf8)
            try? result.write(to: URL(fileURLWithPath: path), options: .atomic)
        }.resume()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}
