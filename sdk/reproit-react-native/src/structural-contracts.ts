export type ContractStatus = 'VIOLATION' | 'SATISFIED' | 'ABSTAIN';
export type StateBoundary =
  | 'rotation'
  | 'background-foreground'
  | 'navigation-round-trip'
  | 'process-recreation';

export interface StructuralObservation {
  key: string;
  state: string;
  authoritative: boolean;
  settled: boolean;
}
export interface StatePreservationContract {
  boundaries: StateBoundary[];
  sample: () => StructuralObservation | null;
  saveBaseline?: (boundary: StateBoundary, value: StructuralObservation) => boolean;
  loadBaseline?: (boundary: StateBoundary) => StructuralObservation | null;
}
export interface ContractResult {
  status: ContractStatus;
  id: string;
  message?: string;
}

export class StatePreservationOracle {
  private contracts = new Map<string, StatePreservationContract>();
  private baselines = new Map<string, StructuralObservation>();
  register(id: string, contract: StatePreservationContract): void {
    if (id && contract.boundaries.length) this.contracts.set(id, contract);
  }
  clear(): void {
    this.contracts.clear();
    this.baselines.clear();
  }
  boundary(kind: StateBoundary, phase: 'before' | 'after'): ContractResult[] {
    const out: ContractResult[] = [];
    for (const [id, c] of [...this.contracts].sort(([a], [b]) => a.localeCompare(b))) {
      if (!c.boundaries.includes(kind)) continue;
      const identity = `state-preservation:${kind}:${id}`;
      if (phase === 'before') {
        const value = safeSample(c.sample);
        if (!valid(value)) {
          out.push({ status: 'ABSTAIN', id: identity });
          continue;
        }
        this.baselines.set(`${kind}:${id}`, value!);
        if (
          kind === 'process-recreation' &&
          (!c.saveBaseline || c.saveBaseline(kind, value!) !== true)
        ) {
          this.baselines.delete(`${kind}:${id}`);
          out.push({ status: 'ABSTAIN', id: identity });
        } else out.push({ status: 'SATISFIED', id: identity });
        continue;
      }
      const before =
        kind === 'process-recreation'
          ? c.loadBaseline
            ? safeSample(() => c.loadBaseline!(kind))
            : null
          : (this.baselines.get(`${kind}:${id}`) ?? null);
      const after = safeSample(c.sample);
      this.baselines.delete(`${kind}:${id}`);
      if (!valid(before) || !valid(after)) {
        out.push({ status: 'ABSTAIN', id: identity });
        continue;
      }
      if (before!.key === after!.key && before!.state === after!.state)
        out.push({ status: 'SATISFIED', id: identity });
      else
        out.push({
          status: 'VIOLATION',
          id: identity,
          message: `declared structural state was not preserved across ${kind}`,
        });
    }
    return out;
  }
}

export interface ActionEffectObservation {
  route?: string;
  state?: string;
  authoritative: boolean;
  settled: boolean;
}
export interface ActionEffectContract {
  sample: () => ActionEffectObservation | null;
  route?: { target: string };
  state?: { target?: string; changed?: boolean };
}
export class ActionEffectOracle {
  private contracts = new Map<string, ActionEffectContract>();
  private before = new Map<string, ActionEffectObservation>();
  register(id: string, contract: ActionEffectContract): void {
    if (id) this.contracts.set(id, contract);
  }
  clear(): void {
    this.contracts.clear();
    this.before.clear();
  }
  begin(id: string): ContractResult[] {
    const c = this.contracts.get(id);
    const identity = `action-effect:${id}`;
    if (!c) return [{ status: 'ABSTAIN', id: identity }];
    const value = safeSample(c.sample);
    if (!validEffect(value)) return [{ status: 'ABSTAIN', id: identity }];
    this.before.set(id, value!);
    return [{ status: 'SATISFIED', id: identity }];
  }
  end(id: string): ContractResult[] {
    const c = this.contracts.get(id);
    const before = this.before.get(id) ?? null;
    this.before.delete(id);
    const after = c ? safeSample(c.sample) : null;
    if (!c || !validEffect(before) || !validEffect(after))
      return [{ status: 'ABSTAIN', id: `action-effect:${id}` }];
    const out: ContractResult[] = [];
    checkTarget(out, id, 'route', c.route, before!.route, after!.route);
    checkChange(out, id, 'state', c.state, before!.state, after!.state);
    return out.length ? out : [{ status: 'ABSTAIN', id: `action-effect:${id}` }];
  }
}

export function contractMarker(results: ContractResult[]): string | null {
  const items = results
    .filter((r) => r.status === 'VIOLATION')
    .map((r) => ({ id: r.id, message: r.message ?? r.id }));
  return items.length ? `REPROIT_INVARIANT ${JSON.stringify({ sig: '', items })}` : null;
}
function safeSample<T>(sample: () => T | null): T | null {
  try {
    return sample();
  } catch {
    return null;
  }
}
function valid(o: StructuralObservation | null): o is StructuralObservation {
  return !!o && o.authoritative && o.settled && !!o.key && !!o.state;
}
function validEffect(o: ActionEffectObservation | null): o is ActionEffectObservation {
  return !!o && o.authoritative && o.settled;
}
function checkTarget(
  out: ContractResult[],
  id: string,
  kind: string,
  expected: { target: string } | undefined,
  _before: string | undefined,
  after: string | undefined,
): void {
  if (!expected) return;
  const identity = `action-effect:${id}:${kind}`;
  if (!expected.target || after === undefined) out.push({ status: 'ABSTAIN', id: identity });
  else if (after === expected.target) out.push({ status: 'SATISFIED', id: identity });
  else
    out.push({ status: 'VIOLATION', id: identity, message: `declared ${kind} effect did not occur` });
}
function checkChange(
  out: ContractResult[],
  id: string,
  kind: string,
  expected: { target?: string; changed?: boolean } | undefined,
  before: string | undefined,
  after: string | undefined,
): void {
  if (!expected) return;
  const identity = `action-effect:${id}:${kind}`;
  if (
    after === undefined ||
    (expected.target === undefined && (expected.changed === undefined || before === undefined))
  ) {
    out.push({ status: 'ABSTAIN', id: identity });
    return;
  }
  const ok =
    expected.target !== undefined
      ? after === expected.target
      : (after !== before) === expected.changed;
  out.push(
    ok
      ? { status: 'SATISFIED', id: identity }
      : { status: 'VIOLATION', id: identity, message: `declared ${kind} effect did not occur` },
  );
}
