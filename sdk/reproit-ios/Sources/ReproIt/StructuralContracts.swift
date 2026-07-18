import Foundation

public enum ReproItContractStatus: String {
  case violation = "VIOLATION"
  case satisfied = "SATISFIED"
  case abstain = "ABSTAIN"
}
public enum ReproItStateBoundary: String {
  case rotation
  case backgroundForeground = "background-foreground"
  case navigationRoundTrip = "navigation-round-trip"
  case processRecreation = "process-recreation"
}
public enum ReproItBoundaryPhase { case before, after }
public struct ReproItStructuralObservation {
  public let key: String, state: String, authoritative: Bool, settled: Bool
  public init(key: String, state: String, authoritative: Bool, settled: Bool) {
    self.key = key
    self.state = state
    self.authoritative = authoritative
    self.settled = settled
  }
}
public struct ReproItContractResult {
  public let status: ReproItContractStatus, id: String, message: String?
}
public struct ReproItStatePreservationContract {
  public let boundaries: Set<ReproItStateBoundary>, sample: () -> ReproItStructuralObservation?
  public let saveBaseline: ((ReproItStateBoundary, ReproItStructuralObservation) -> Bool)?
  public let loadBaseline: ((ReproItStateBoundary) -> ReproItStructuralObservation?)?
  public init(
    boundaries: Set<ReproItStateBoundary>, sample: @escaping () -> ReproItStructuralObservation?,
    saveBaseline: ((ReproItStateBoundary, ReproItStructuralObservation) -> Bool)? = nil,
    loadBaseline: ((ReproItStateBoundary) -> ReproItStructuralObservation?)? = nil
  ) {
    self.boundaries = boundaries
    self.sample = sample
    self.saveBaseline = saveBaseline
    self.loadBaseline = loadBaseline
  }
}
enum ReproItStatePreservationContracts {
  static var contracts: [String: ReproItStatePreservationContract] = [:],
    baselines: [String: ReproItStructuralObservation] = [:]
  static func register(_ id: String, _ c: ReproItStatePreservationContract) {
    if !id.isEmpty && !c.boundaries.isEmpty { contracts[id] = c }
  }
  static func clear() {
    contracts.removeAll()
    baselines.removeAll()
  }
  static func boundary(_ kind: ReproItStateBoundary, _ phase: ReproItBoundaryPhase)
    -> [ReproItContractResult]
  {
    var out: [ReproItContractResult] = []
    for (id, c) in contracts.sorted(by: { $0.key < $1.key }) where c.boundaries.contains(kind) {
      let identity = "state-preservation:\(kind.rawValue):\(id)"
      let key = "\(kind.rawValue):\(id)"
      if phase == .before {
        let value = sample(c.sample)
        guard valid(value) else {
          out.append(abstain(identity))
          continue
        }
        baselines[key] = value!
        if kind == .processRecreation
          && (c.saveBaseline == nil || safe { c.saveBaseline!(kind, value!) } != true)
        {
          baselines[key] = nil
          out.append(abstain(identity))
        } else {
          out.append(satisfiedResult(identity))
        }
        continue
      }
      let before =
        kind == .processRecreation
        ? (c.loadBaseline.flatMap { f in sample { f(kind) } }) : baselines[key]
      let after = sample(c.sample)
      baselines[key] = nil
      guard valid(before), valid(after) else {
        out.append(abstain(identity))
        continue
      }
      if before!.key == after!.key && before!.state == after!.state {
        out.append(satisfiedResult(identity))
      } else {
        out.append(
          violation(identity, "declared structural state was not preserved across \(kind.rawValue)"))
      }
    }
    return out
  }
}

public struct ReproItActionEffectObservation {
  public let route: String?, state: String?, authoritative: Bool, settled: Bool
  public init(route: String? = nil, state: String? = nil, authoritative: Bool, settled: Bool) {
    self.route = route
    self.state = state
    self.authoritative = authoritative
    self.settled = settled
  }
}
public struct ReproItTargetEffect {
  public let target: String
  public init(_ target: String) { self.target = target }
}
public struct ReproItChangeEffect {
  public let target: String?, changed: Bool?
  public init(target: String? = nil, changed: Bool? = nil) {
    self.target = target
    self.changed = changed
  }
}
public struct ReproItActionEffectContract {
  public let sample: () -> ReproItActionEffectObservation?, route: ReproItTargetEffect?,
    state: ReproItChangeEffect?
  public init(
    sample: @escaping () -> ReproItActionEffectObservation?, route: ReproItTargetEffect? = nil,
    state: ReproItChangeEffect? = nil
  ) {
    self.sample = sample
    self.route = route
    self.state = state
  }
}
enum ReproItActionEffectContracts {
  static var contracts: [String: ReproItActionEffectContract] = [:],
    before: [String: ReproItActionEffectObservation] = [:]
  static func register(_ id: String, _ c: ReproItActionEffectContract) {
    if !id.isEmpty { contracts[id] = c }
  }
  static func clear() {
    contracts.removeAll()
    before.removeAll()
  }
  static func begin(_ id: String) -> [ReproItContractResult] {
    guard let c = contracts[id], let value = sampleEffect(c.sample), valid(value) else {
      return [abstain("action-effect:\(id)")]
    }
    before[id] = value
    return [satisfiedResult("action-effect:\(id)")]
  }
  static func end(_ id: String) -> [ReproItContractResult] {
    guard let c = contracts[id], let old = before.removeValue(forKey: id),
      let now = sampleEffect(c.sample), valid(old), valid(now)
    else { return [abstain("action-effect:\(id)")] }
    var out: [ReproItContractResult] = []
    if let e = c.route { checkTarget(&out, id, "route", e.target, now.route) }
    if let e = c.state { checkChange(&out, id, "state", e, old.state, now.state) }
    return out.isEmpty ? [abstain("action-effect:\(id)")] : out
  }
}
func reproitContractMarker(_ results: [ReproItContractResult]) -> String? {
  let items = results.filter { $0.status == .violation }.map {
    ["id": $0.id, "message": $0.message ?? $0.id]
  }
  guard !items.isEmpty,
    let d = try? JSONSerialization.data(withJSONObject: ["sig": "", "items": items]),
    let j = String(data: d, encoding: .utf8)
  else { return nil }
  return "REPROIT_INVARIANT \(j)"
}
private func sample(_ f: () -> ReproItStructuralObservation?) -> ReproItStructuralObservation? {
  f()
}
private func sampleEffect(_ f: () -> ReproItActionEffectObservation?)
  -> ReproItActionEffectObservation?
{ f() }
private func safe(_ f: () -> Bool) -> Bool { f() }
private func valid(_ o: ReproItStructuralObservation?) -> Bool {
  o != nil && o!.authoritative && o!.settled && !o!.key.isEmpty && !o!.state.isEmpty
}
private func valid(_ o: ReproItActionEffectObservation?) -> Bool {
  o != nil && o!.authoritative && o!.settled
}
private func abstain(_ id: String) -> ReproItContractResult {
  .init(status: .abstain, id: id, message: nil)
}
private func satisfiedResult(_ id: String) -> ReproItContractResult {
  .init(status: .satisfied, id: id, message: nil)
}
private func violation(_ id: String, _ message: String) -> ReproItContractResult {
  .init(status: .violation, id: id, message: message)
}
private func checkTarget(
  _ out: inout [ReproItContractResult], _ id: String, _ kind: String, _ target: String,
  _ after: String?
) {
  let identity = "action-effect:\(id):\(kind)"
  guard !target.isEmpty, let after = after else {
    out.append(abstain(identity))
    return
  }
  out.append(
    after == target
      ? satisfiedResult(identity) : violation(identity, "declared \(kind) effect did not occur"))
}
private func checkChange(
  _ out: inout [ReproItContractResult], _ id: String, _ kind: String, _ e: ReproItChangeEffect,
  _ before: String?, _ after: String?
) {
  let identity = "action-effect:\(id):\(kind)"
  guard let after = after, e.target != nil || (e.changed != nil && before != nil) else {
    out.append(abstain(identity))
    return
  }
  let ok = e.target != nil ? after == e.target! : (after != before!) == e.changed!
  out.append(ok ? satisfiedResult(identity) : violation(identity, "declared \(kind) effect did not occur"))
}
