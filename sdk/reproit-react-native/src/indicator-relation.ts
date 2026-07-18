export interface ReproItRect {
  x: number;
  y: number;
  width: number;
  height: number;
}
export interface ReproItIndicatorGeometry {
  indicator: ReproItRect;
  owner: ReproItRect;
  container: ReproItRect;
  animating?: boolean;
  transformsResolved?: boolean;
}
export interface ReproItIndicatorContract {
  dependentKey: string;
  ownerKey: string;
  containerKey: string;
  maxGap?: number;
  sample: () => ReproItIndicatorGeometry | null;
}
type Stored = ReproItIndicatorContract & { maxGap: number };
export class IndicatorRelations {
  private contracts = new Map<string, Stored>();
  private prior = new Map<string, string>();
  private counts = new Map<string, number>();
  register(id: string, c: ReproItIndicatorContract): void {
    const gap = c.maxGap ?? 8;
    if (
      !id ||
      !c.dependentKey ||
      !c.ownerKey ||
      !c.containerKey ||
      !Number.isFinite(gap) ||
      gap < 0
    )
      return;
    this.contracts.set(id, { ...c, maxGap: gap });
  }
  clear(): void {
    this.contracts.clear();
    this.prior.clear();
    this.counts.clear();
  }
  marker(): string | null {
    const checks: Record<string, string>[] = [];
    for (const [id, c] of [...this.contracts.entries()].sort(([a], [b]) => a.localeCompare(b))) {
      const r = this.evaluate(c);
      const count = this.prior.get(id) === r.fp ? (this.counts.get(id) ?? 0) + 1 : 1;
      this.prior.set(id, r.fp);
      this.counts.set(id, count);
      if (count < 2) continue;
      checks.push({
        kind: 'indicator-anchor',
        dependentKey: c.dependentKey,
        ownerKey: c.ownerKey,
        containerKey: c.containerKey,
        outcome: r.outcome,
        ...(r.violation ? { violation: r.violation } : {}),
      });
    }
    return checks.length
      ? `REPROIT_RELATION ${JSON.stringify({ stableSamples: 2, checks })}`
      : null;
  }
  private evaluate(c: Stored): { outcome: string; violation?: string; fp: string } {
    let g: ReproItIndicatorGeometry | null = null;
    try {
      g = c.sample();
    } catch {
      g = null;
    }
    if (
      !g ||
      g.animating ||
      g.transformsResolved === false ||
      !valid(g.indicator) ||
      !valid(g.owner) ||
      !valid(g.container)
    )
      return { outcome: 'ABSTAIN', fp: 'ABSTAIN' };
    const i = edges(g.indicator),
      o = edges(g.owner),
      box = edges(g.container);
    const escaped =
      i.l < box.l - 0.5 || i.t < box.t - 0.5 || i.r > box.r + 0.5 || i.b > box.b + 0.5;
    const dx = Math.max(0, o.l - i.r, i.l - o.r),
      dy = Math.max(0, o.t - i.b, i.t - o.b);
    const violation = escaped
      ? 'escaped-container'
      : Math.hypot(dx, dy) > c.maxGap + 0.5
        ? 'detached'
        : undefined;
    const fp =
      [g.indicator, g.owner, g.container]
        .flatMap((r) => [r.x, r.y, r.width, r.height])
        .map((v) => Math.round(v * 2))
        .join(',') +
      '|' +
      (violation ?? 'valid');
    return { outcome: violation ? 'VIOLATION' : 'SATISFIED', violation, fp };
  }
}
function valid(r: ReproItRect): boolean {
  return [r.x, r.y, r.width, r.height].every(Number.isFinite) && r.width > 0 && r.height > 0;
}
function edges(r: ReproItRect) {
  return { l: r.x, t: r.y, r: r.x + r.width, b: r.y + r.height };
}
