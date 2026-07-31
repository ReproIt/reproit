import Foundation
import UIKit
import ReproIt

// Simulator harness for production exchange capture.
//
// Phase is chosen by environment so one binary proves both directions:
//   RP_PHASE=capture  mount the SDK with captureExchanges, call the live stub
//                     dependency, then trigger a planted failure.
//   RP_PHASE=replay   mount under the runner capsule contract with the stub
//                     DOWN, and prove the recorded response is served.
//   RP_PHASE=miss     same, but call a URL the capsule does not hold.
//
// Every outcome is printed with an RP: prefix so simctl launch --console can
// be parsed without ambiguity.

func line(_ text: String) {
  print("RP: \(text)")
  fflush(stdout)
}

final class AppDelegate: NSObject, UIApplicationDelegate {
  func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions options: [UIApplication.LaunchOptionsKey: Any]? = nil
  ) -> Bool {
    run()
    return true
  }

  func run() {
    let env = ProcessInfo.processInfo.environment
    let phase = env["RP_PHASE"] ?? "capture"
    let dependency = env["RP_DEPENDENCY"] ?? "http://127.0.0.1:19801/prices?tier=gold"
    let ingest = env["RP_INGEST"] ?? "http://127.0.0.1:19802"

    line("phase=\(phase)")
    line("dependency=\(dependency)")

    let config = ReproItConfig(
      appId: "sim-app",
      endpoint: ingest,
      apiKey: "sk_live_simulator",
      buildVersion: "1.4.2",
      buildCommit: "abc123def456",
      captureExchanges: phase == "capture")
    guard let engine = ReproIt.start(config) else {
      line("result=ENGINE-NIL")
      exit(6)
    }

    // A real URLSession request through the device's networking stack.
    let target = phase == "miss"
      ? (env["RP_UNMATCHED"] ?? "http://127.0.0.1:19801/unknown-endpoint")
      : dependency
    let semaphore = DispatchSemaphore(value: 0)
    var payload: [String: Any]?
    var failure: String?

    URLSession.shared.dataTask(with: URL(string: target)!) { data, response, error in
      if let error {
        failure = "\(error)"
      } else if let data {
        let status = (response as? HTTPURLResponse)?.statusCode ?? -1
        line("http-status=\(status)")
        payload = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        line("http-body=\(String(data: data, encoding: .utf8) ?? "<binary>")")
      }
      semaphore.signal()
    }.resume()

    if semaphore.wait(timeout: .now() + 20) == .timedOut {
      line("result=TIMEOUT")
      exit(3)
    }

    if let failure {
      line("network-error=\(failure)")
      line("result=NETWORK-FAILED")
      exit(4)
    }

    // The planted failure: the handler assumes `prices` is an array, and the
    // dependency returned null. This is the same planted shape the backend
    // fixtures use.
    let prices = payload?["prices"] as? [Any]
    if prices == nil {
      let message = "TypeError: prices is not an array"
      line("planted-failure=\(message)")
      engine.recordError(
        message: message, stack: ["quote.swift:41"], source: "quote.swift", line: 41)
      // Give the capsule POST time to leave the device.
      Thread.sleep(forTimeInterval: 3.0)
      line("result=FAILED-AS-PLANTED")
      exit(0)
    }

    line("result=OK-UNEXPECTED")
    exit(5)
  }
}

let delegate = AppDelegate()
UIApplicationMain(
  CommandLine.argc,
  CommandLine.unsafeArgv,
  nil,
  NSStringFromClass(AppDelegate.self))
