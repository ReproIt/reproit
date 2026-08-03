// ReproIt iOS smoke fixture: the tiny, deterministic target the CI iOS/Appium
// smoke (.github/scripts/appium-ios-smoke.sh) drives instead of the
// preinstalled Settings app. Settings' accessibility tree is iOS-version and
// boot-timing dependent (on GitHub macos-15 runners the first XCUITest
// snapshot of Settings can come back with zero structural elements), so the
// smoke now builds and drives this app: a single screen whose controls carry
// explicit accessibilityIdentifiers, so the runner's structural snapshot is
// guaranteed a non-empty elements list, and whose one button structurally
// changes the screen (adds/removes an identified label), so a tap provably
// produces a new canonical signature (EXPLORE:EDGE).
//
// UIKit + programmatic UI + classic delegate lifecycle on purpose: the whole
// app is this one file, compiled directly with swiftc against the simulator
// SDK by build.sh (no Xcode project, no signing, no storyboard).
import UIKit

final class FixtureViewController: UIViewController {
  private let stack = UIStackView()
  private let titleLabel = UILabel()
  private let toggleButton = UIButton(type: .system)
  private let detailLabel = UILabel()
  private var revealed = false

  override func viewDidLoad() {
    super.viewDidLoad()
    view.backgroundColor = .systemBackground

    titleLabel.text = "ReproIt Smoke Fixture"
    titleLabel.font = .preferredFont(forTextStyle: .headline)
    titleLabel.accessibilityIdentifier = "fixture.title"

    toggleButton.setTitle("Reveal detail", for: .normal)
    toggleButton.accessibilityIdentifier = "fixture.toggle"
    toggleButton.addTarget(self, action: #selector(toggleTapped), for: .touchUpInside)

    // Not installed until the first tap: its presence/absence is the
    // structural state change the smoke asserts on (a text node with a
    // stable id enters/leaves the canonical tree).
    detailLabel.text = "Detail revealed"
    detailLabel.accessibilityIdentifier = "fixture.detail"

    stack.axis = .vertical
    stack.alignment = .center
    stack.spacing = 24
    stack.translatesAutoresizingMaskIntoConstraints = false
    stack.addArrangedSubview(titleLabel)
    stack.addArrangedSubview(toggleButton)
    view.addSubview(stack)
    NSLayoutConstraint.activate([
      stack.centerXAnchor.constraint(equalTo: view.centerXAnchor),
      stack.centerYAnchor.constraint(equalTo: view.centerYAnchor),
    ])
  }

  @objc private func toggleTapped() {
    revealed.toggle()
    if revealed {
      stack.addArrangedSubview(detailLabel)
      toggleButton.setTitle("Hide detail", for: .normal)
    } else {
      stack.removeArrangedSubview(detailLabel)
      detailLabel.removeFromSuperview()
      toggleButton.setTitle("Reveal detail", for: .normal)
    }
  }
}

final class AppDelegate: UIResponder, UIApplicationDelegate {
  var window: UIWindow?

  func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    let w = UIWindow(frame: UIScreen.main.bounds)
    w.rootViewController = FixtureViewController()
    w.makeKeyAndVisible()
    window = w
    return true
  }
}

// Top-level entry (this file is main.swift, so no @main attribute).
UIApplicationMain(
  CommandLine.argc, CommandLine.unsafeArgv, nil, NSStringFromClass(AppDelegate.self))
