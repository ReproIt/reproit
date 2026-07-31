import Foundation
import UIKit
import WebKit

// WKWebView host for the React Native SDK's capture path.
//
// The webview runs the SDK's REAL built dist modules against WebKit's genuine
// fetch, so device networking, bounds, redaction, and capture-batch emission
// are exercised on the simulator. What this host does NOT exercise is stated
// in validation/MEASUREMENT.md: the React provider and the NativeModules
// bridge, which need a full RN app.

func line(_ text: String) {
  print(text)
  fflush(stdout)
}

final class Bridge: NSObject, WKScriptMessageHandler {
  func userContentController(
    _ controller: WKUserContentController, didReceive message: WKScriptMessage
  ) {
    guard let text = message.body as? String else { return }
    line(text)
    if text.contains("result=") || text.contains("harness-error=") {
      DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { exit(0) }
    }
  }
}

final class AppDelegate: NSObject, UIApplicationDelegate {
  var window: UIWindow?
  var webView: WKWebView?
  let bridge = Bridge()

  func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]? = nil
  ) -> Bool {
    let env = ProcessInfo.processInfo.environment
    let phase = env["RP_PHASE"] ?? "capture"
    let dependency = env["RP_DEPENDENCY"] ?? ""
    let ingest = env["RP_INGEST"] ?? ""
    let unmatched = env["RP_UNMATCHED"] ?? ""
    let capsule = env["RP_CAPSULE"] ?? "{}"
    let bundlePath = env["RP_BUNDLE"] ?? ""

    let config = WKWebViewConfiguration()
    config.userContentController.add(bridge, name: "rp")
    // The SDK's modules and the harness are injected as page scripts, so the
    // code under test is the published dist, unmodified.
    if let loader = try? String(contentsOfFile: bundlePath + "/loader.js", encoding: .utf8),
      let harness = try? String(contentsOfFile: bundlePath + "/harness.js", encoding: .utf8)
    {
      config.userContentController.addUserScript(
        WKUserScript(source: loader, injectionTime: .atDocumentStart, forMainFrameOnly: true))
      config.userContentController.addUserScript(
        WKUserScript(source: harness, injectionTime: .atDocumentEnd, forMainFrameOnly: true))
    } else {
      line("RP: harness-error=could not read loader/harness from \(bundlePath)")
      DispatchQueue.main.asyncAfter(deadline: .now() + 1) { exit(7) }
      return true
    }

    let view = WKWebView(frame: .zero, configuration: config)
    webView = view
    let window = UIWindow(frame: UIScreen.main.bounds)
    window.rootViewController = UIViewController()
    window.rootViewController?.view = view
    window.makeKeyAndVisible()
    self.window = window

    var components = URLComponents(string: "http://127.0.0.1:\(env["RP_PAGE_PORT"] ?? "19803")/")!
    components.queryItems = [
      URLQueryItem(name: "phase", value: phase),
      URLQueryItem(name: "dependency", value: dependency),
      URLQueryItem(name: "ingest", value: ingest),
      URLQueryItem(name: "unmatched", value: unmatched),
      URLQueryItem(name: "capsule", value: capsule.addingPercentEncoding(
        withAllowedCharacters: .alphanumerics) ?? "%7B%7D"),
    ]
    view.load(URLRequest(url: components.url!))
    return true
  }
}

let delegate = AppDelegate()
UIApplicationMain(
  CommandLine.argc, CommandLine.unsafeArgv, nil, NSStringFromClass(AppDelegate.self))
