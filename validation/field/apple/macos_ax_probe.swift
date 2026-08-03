import ApplicationServices
import Cocoa
import Foundation

private enum ProbeError: Error, CustomStringConvertible {
  case usage
  case appNotRunning(String)
  case elementNotFound(String)
  case attributeUnavailable(String)

  var description: String {
    switch self {
    case .usage:
      return "usage: macos_ax_probe <bundle-id> <element-title> <attribute>"
    case .appNotRunning(let bundleID):
      return "application is not running: \(bundleID)"
    case .elementNotFound(let title):
      return "accessibility element was not found: \(title)"
    case .attributeUnavailable(let attribute):
      return "accessibility attribute is unavailable: \(attribute)"
    }
  }
}

private func copyAttribute(_ element: AXUIElement, _ attribute: String) -> AnyObject? {
  var value: CFTypeRef?
  let result = AXUIElementCopyAttributeValue(element, attribute as CFString, &value)
  return result == .success ? value : nil
}

private func stringAttribute(_ element: AXUIElement, _ attribute: String) -> String? {
  copyAttribute(element, attribute) as? String
}

private func children(_ element: AXUIElement) -> [AXUIElement] {
  copyAttribute(element, kAXChildrenAttribute as String) as? [AXUIElement] ?? []
}

private func findElement(
  root: AXUIElement,
  title: String,
  maximumElements: Int = 10_000
) -> AXUIElement? {
  var pending = [root]
  var nextIndex = 0
  var roleMatches = 0
  let roleSelector: (role: String, index: Int)? = {
    guard title.hasPrefix("role:"),
      let separator = title.lastIndex(of: "#"),
      let index = Int(title[title.index(after: separator)...])
    else {
      return nil
    }
    return (String(title[title.index(title.startIndex, offsetBy: 5)..<separator]), index)
  }()

  while nextIndex < pending.count && nextIndex < maximumElements {
    let element = pending[nextIndex]
    nextIndex += 1

    if let selector = roleSelector,
      stringAttribute(element, kAXRoleAttribute as String) == selector.role
    {
      if roleMatches == selector.index {
        return element
      }
      roleMatches += 1
    }

    let candidateTitles = [
      stringAttribute(element, kAXTitleAttribute as String),
      stringAttribute(element, kAXDescriptionAttribute as String),
      stringAttribute(element, kAXValueAttribute as String),
    ]
    if candidateTitles.compactMap({ $0 }).contains(title) {
      return element
    }
    pending.append(contentsOf: children(element))
  }
  return nil
}

private func dumpElements(root: AXUIElement, maximumElements: Int = 10_000) throws {
  var pending = [root]
  var nextIndex = 0
  var output: [[String: String]] = []

  while nextIndex < pending.count && nextIndex < maximumElements {
    let element = pending[nextIndex]
    nextIndex += 1
    let attributes = [
      "role": stringAttribute(element, kAXRoleAttribute as String),
      "title": stringAttribute(element, kAXTitleAttribute as String),
      "description": stringAttribute(element, kAXDescriptionAttribute as String),
      "help": stringAttribute(element, kAXHelpAttribute as String),
      "value": stringAttribute(element, kAXValueAttribute as String),
    ].compactMapValues { $0 }
    if attributes.count > 1 {
      output.append(attributes)
    }
    pending.append(contentsOf: children(element))
  }

  let data = try JSONSerialization.data(
    withJSONObject: output,
    options: [.prettyPrinted, .sortedKeys]
  )
  FileHandle.standardOutput.write(data)
  FileHandle.standardOutput.write(Data([0x0A]))
}

private func run() throws {
  guard CommandLine.arguments.count == 4 else {
    throw ProbeError.usage
  }
  guard AXIsProcessTrusted() else {
    throw ProbeError.attributeUnavailable("Accessibility permission")
  }

  let bundleID = CommandLine.arguments[1]
  let title = CommandLine.arguments[2]
  let attribute = CommandLine.arguments[3]
  guard let application = NSRunningApplication.runningApplications(
    withBundleIdentifier: bundleID
  ).first else {
    throw ProbeError.appNotRunning(bundleID)
  }

  let root = AXUIElementCreateApplication(application.processIdentifier)
  if title == "--dump" {
    try dumpElements(root: root)
    return
  }
  guard let element = findElement(root: root, title: title) else {
    throw ProbeError.elementNotFound(title)
  }
  if attribute == "AXPress" {
    let result = AXUIElementPerformAction(element, kAXPressAction as CFString)
    guard result == .success else {
      throw ProbeError.attributeUnavailable(attribute)
    }
    let output: [String: Any] = [
      "action": attribute,
      "bundleID": bundleID,
      "elementTitle": title,
      "result": "success",
    ]
    let data = try JSONSerialization.data(withJSONObject: output, options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0A]))
    return
  }
  guard let value = copyAttribute(element, attribute) else {
    throw ProbeError.attributeUnavailable(attribute)
  }

  let role = stringAttribute(element, kAXRoleAttribute as String) ?? ""
  let output: [String: Any] = [
    "attribute": attribute,
    "bundleID": bundleID,
    "elementTitle": title,
    "role": role,
    "value": String(describing: value),
  ]
  let data = try JSONSerialization.data(withJSONObject: output, options: [.sortedKeys])
  FileHandle.standardOutput.write(data)
  FileHandle.standardOutput.write(Data([0x0A]))
}

do {
  try run()
} catch {
  FileHandle.standardError.write("macos AX probe: \(error)\n".data(using: .utf8)!)
  exit(1)
}
