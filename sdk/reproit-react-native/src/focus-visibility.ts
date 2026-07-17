import type { ReproItRect } from './indicator-relation';

export interface FocusVisibilityObservation {
  key: string;
  focusedEditable: boolean;
  field: ReproItRect;
  usableViewport: ReproItRect;
  exactKeyboardRect: boolean;
  animating?: boolean;
  transformsResolved?: boolean;
  intentionalHiddenEditor?: boolean;
  systemUi?: boolean;
}
export interface FocusVisibilityContract {
  sample: () => FocusVisibilityObservation | null;
  /** Must use the owning framework scroll container. False means abstain. */
  reveal: () => boolean;
}

export class FocusVisibilityOracle {
  private contracts = new Map<string, FocusVisibilityContract>();
  private attempted = new Set<string>();
  private prior = new Map<string, string>();
  private counts = new Map<string, number>();
  register(id: string, c: FocusVisibilityContract): void {
    if (id) this.contracts.set(id, c);
  }
  clear(): void {
    this.contracts.clear();
    this.attempted.clear();
    this.prior.clear();
    this.counts.clear();
  }
  marker(): string | null {
    const items: Array<{ id: string; message: string }> = [];
    for (const [id, c] of [...this.contracts].sort(([a], [b]) => a.localeCompare(b))) {
      let o: FocusVisibilityObservation | null = null;
      try {
        o = c.sample();
      } catch {
        o = null;
      }
      if (!valid(o)) {
        this.reset(id);
        continue;
      }
      if (intersects(o!.field, o!.usableViewport)) {
        this.reset(id);
        continue;
      }
      if (!this.attempted.has(id)) {
        let safe = false;
        try {
          safe = c.reveal() === true;
        } catch {
          safe = false;
        }
        if (!safe) {
          this.reset(id);
          continue;
        }
        this.attempted.add(id);
        this.prior.delete(id);
        this.counts.delete(id);
        continue;
      }
      const fp = [o!.field, o!.usableViewport]
        .flatMap((r) => [r.x, r.y, r.width, r.height])
        .map((v) => Math.round(v * 2))
        .join(',');
      const n = this.prior.get(id) === fp ? (this.counts.get(id) ?? 0) + 1 : 1;
      this.prior.set(id, fp);
      this.counts.set(id, n);
      if (n >= 2)
        items.push({
          id: `focused-input-obscured:${o!.key}`,
          message:
            'focused editable has no usable visible rectangle after its owning ' +
            'scroll container attempted reveal',
        });
    }
    return items.length ? `REPROIT_INVARIANT ${JSON.stringify({ sig: '', items })}` : null;
  }
  private reset(id: string): void {
    this.attempted.delete(id);
    this.prior.delete(id);
    this.counts.delete(id);
  }
}
function valid(o: FocusVisibilityObservation | null): o is FocusVisibilityObservation {
  return (
    !!o &&
    !!o.key &&
    o.focusedEditable &&
    o.exactKeyboardRect &&
    !o.animating &&
    o.transformsResolved !== false &&
    !o.intentionalHiddenEditor &&
    !o.systemUi &&
    rect(o.field) &&
    rect(o.usableViewport)
  );
}
function rect(r: ReproItRect): boolean {
  return [r.x, r.y, r.width, r.height].every(Number.isFinite) && r.width > 0 && r.height > 0;
}
function intersects(a: ReproItRect, b: ReproItRect): boolean {
  return (
    Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x) > 0.5 &&
    Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y) > 0.5
  );
}
