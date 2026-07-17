import Foundation

#if canImport(CoreGraphics)
  import CoreGraphics

  /// Screen-coordinate geometry for one explicit indicator ownership contract.
  /// `animating` and `transformsResolved` are fail-closed gates: either one makes
  /// the oracle abstain.
  public struct ReproItIndicatorGeometry {
    public let indicator: CGRect
    public let owner: CGRect
    public let container: CGRect
    public let animating: Bool
    public let transformsResolved: Bool

    public init(
      indicator: CGRect, owner: CGRect, container: CGRect,
      animating: Bool = false, transformsResolved: Bool = true
    ) {
      self.indicator = indicator
      self.owner = owner
      self.container = container
      self.animating = animating
      self.transformsResolved = transformsResolved
    }
  }

  enum ReproItIndicatorRelations {
    struct Contract {
      let dependentKey: String
      let ownerKey: String
      let containerKey: String
      let maxGap: CGFloat
      let sample: () -> ReproItIndicatorGeometry?
    }
    private static let lock = NSLock()
    private static var contracts: [String: Contract] = [:]
    private static var prior: [String: String] = [:]
    private static var counts: [String: Int] = [:]

    static func register(
      _ id: String, dependentKey: String, ownerKey: String,
      containerKey: String, maxGap: CGFloat,
      sample: @escaping () -> ReproItIndicatorGeometry?
    ) {
      guard !id.isEmpty, !dependentKey.isEmpty, !ownerKey.isEmpty,
        !containerKey.isEmpty, maxGap.isFinite, maxGap >= 0
      else { return }
      lock.lock()
      contracts[id] = Contract(
        dependentKey: dependentKey,
        ownerKey: ownerKey, containerKey: containerKey, maxGap: maxGap,
        sample: sample)
      lock.unlock()
    }

    static func clear() {
      lock.lock()
      contracts.removeAll()
      prior.removeAll()
      counts.removeAll()
      lock.unlock()
    }
    static var hasContracts: Bool {
      lock.lock()
      defer { lock.unlock() }
      return !contracts.isEmpty
    }

    static func marker() -> String? {
      lock.lock()
      let snapshot = contracts
      lock.unlock()
      var checks: [[String: Any]] = []
      for (id, contract) in snapshot.sorted(by: { $0.key < $1.key }) {
        let result = evaluate(contract)
        let fingerprint = result.fingerprint
        lock.lock()
        let count = prior[id] == fingerprint ? (counts[id] ?? 0) + 1 : 1
        prior[id] = fingerprint
        counts[id] = count
        lock.unlock()
        guard count >= 2 else { continue }
        var check: [String: Any] = [
          "kind": "indicator-anchor", "dependentKey": contract.dependentKey,
          "ownerKey": contract.ownerKey, "containerKey": contract.containerKey,
          "outcome": result.outcome,
        ]
        if let violation = result.violation { check["violation"] = violation }
        checks.append(check)
      }
      guard !checks.isEmpty,
        let data = try? JSONSerialization.data(withJSONObject: [
          "stableSamples": 2, "checks": checks,
        ]),
        let json = String(data: data, encoding: .utf8)
      else { return nil }
      return "REPROIT_RELATION \(json)"
    }

    private static func evaluate(_ c: Contract) -> (
      outcome: String, violation: String?, fingerprint: String
    ) {
      guard let g = c.sample(), !g.animating, g.transformsResolved,
        valid(g.indicator), valid(g.owner), valid(g.container)
      else {
        return ("UNKNOWN", nil, "UNKNOWN")
      }
      let i = g.indicator.standardized
      let o = g.owner.standardized
      let box = g.container.standardized
      let escaped = !box.insetBy(dx: -0.5, dy: -0.5).contains(i)
      let dx = max(0, max(o.minX - i.maxX, i.minX - o.maxX))
      let dy = max(0, max(o.minY - i.maxY, i.minY - o.maxY))
      let detached = hypot(dx, dy) > c.maxGap + 0.5
      let violation = escaped ? "escaped-container" : (detached ? "detached" : nil)
      let values: [CGFloat] = [
        i.minX, i.minY, i.width, i.height,
        o.minX, o.minY, o.width, o.height,
        box.minX, box.minY, box.width, box.height,
      ]
      let coordinates = values.map { String(Int(($0 * 2).rounded())) }.joined(separator: ",")
      let fp = coordinates + "|" + (violation ?? "valid")
      return (violation == nil ? "VALID" : "PROVEN", violation, fp)
    }

    private static func valid(_ r: CGRect) -> Bool {
      [r.minX, r.minY, r.width, r.height].allSatisfy(\.isFinite) && r.width > 0 && r.height > 0
    }
  }

  public struct ReproItFocusObservation {
    public let key: String, focusedEditable: Bool, field: CGRect, usableViewport: CGRect
    public let exactKeyboardRect: Bool, animating: Bool, transformsResolved: Bool
    public let intentionalHiddenEditor: Bool, systemUI: Bool
    public init(
      key: String, focusedEditable: Bool, field: CGRect, usableViewport: CGRect,
      exactKeyboardRect: Bool, animating: Bool = false, transformsResolved: Bool = true,
      intentionalHiddenEditor: Bool = false, systemUI: Bool = false
    ) {
      self.key = key
      self.focusedEditable = focusedEditable
      self.field = field
      self.usableViewport = usableViewport
      self.exactKeyboardRect = exactKeyboardRect
      self.animating = animating
      self.transformsResolved = transformsResolved
      self.intentionalHiddenEditor = intentionalHiddenEditor
      self.systemUI = systemUI
    }
  }
  enum ReproItFocusVisibility {
    struct C {
      let sample: () -> ReproItFocusObservation?
      let reveal: () -> Bool
    }
    static var contracts: [String: C] = [:], attempted = Set<String>(),
      prior: [String: String] = [:], counts: [String: Int] = [:]
    static func register(
      _ id: String, sample: @escaping () -> ReproItFocusObservation?, reveal: @escaping () -> Bool
    ) {
      guard !id.isEmpty else { return }
      contracts[id] = C(sample: sample, reveal: reveal)
    }
    static var hasContracts: Bool { !contracts.isEmpty }
    static func clear() {
      contracts.removeAll()
      attempted.removeAll()
      prior.removeAll()
      counts.removeAll()
    }
    static func marker() -> String? {
      var items: [[String: String]] = []
      for (id, c) in contracts.sorted(by: { $0.key < $1.key }) {
        guard let o = c.sample(), valid(o) else {
          reset(id)
          continue
        }
        if o.field.intersects(o.usableViewport) {
          reset(id)
          continue
        }
        if !attempted.contains(id) {
          guard c.reveal() else {
            reset(id)
            continue
          }
          attempted.insert(id)
          prior[id] = nil
          counts[id] = nil
          continue
        }
        let vals: [CGFloat] = [
          o.field.minX, o.field.minY, o.field.width, o.field.height, o.usableViewport.minX,
          o.usableViewport.minY, o.usableViewport.width, o.usableViewport.height,
        ]
        let fp = vals.map { String(Int(($0 * 2).rounded())) }.joined(separator: ",")
        let n = prior[id] == fp ? (counts[id] ?? 0) + 1 : 1
        prior[id] = fp
        counts[id] = n
        if n >= 2 {
          items.append([
            "id": "focused-input-obscured:\(o.key)",
            "message":
              "focused editable has no usable visible rectangle after its owning scroll "
                + "container attempted reveal",
          ])
        }
      }
      guard !items.isEmpty,
        let d = try? JSONSerialization.data(withJSONObject: ["sig": "", "items": items]),
        let j = String(data: d, encoding: .utf8)
      else { return nil }
      return "REPROIT_INVARIANT \(j)"
    }
    static func reset(_ id: String) {
      attempted.remove(id)
      prior[id] = nil
      counts[id] = nil
    }
    static func valid(_ o: ReproItFocusObservation) -> Bool {
      let values = [
        o.field.minX, o.field.minY, o.field.width, o.field.height,
        o.usableViewport.minX, o.usableViewport.minY,
        o.usableViewport.width, o.usableViewport.height,
      ]
      return o.focusedEditable && o.exactKeyboardRect && !o.animating && o.transformsResolved
        && !o.intentionalHiddenEditor && !o.systemUI && !o.key.isEmpty
        && values.allSatisfy(\.isFinite) && o.field.width > 0 && o.field.height > 0
        && o.usableViewport.width > 0 && o.usableViewport.height > 0
    }
  }
#endif
