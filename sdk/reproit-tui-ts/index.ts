// Public entry point for the reproit TUI TypeScript SDK.
//
// The signature core (signature.ts) is a byte-for-byte port of the canonical Rust
// crate crates/tui-sig/src/lib.rs and is pinned to the repo-root golden vectors
// tui_signature_vectors.json. TUI signatures live in a SEPARATE namespace from
// the a11y golden vectors (signature_vectors.json); see README.md.

export {
  sigOf,
  structuralClass,
  skeletonOf,
  structuralSig,
  numericValueClasses,
  valueClass,
  isStrictDecimal,
  contentFingerprint,
  labelsOf,
  MAX_VALUE_CLASSES,
  MAX_LABELS,
} from "./signature.ts";

export { ScreenContents } from "./screen.ts";
export type { Cell, Row, Cursor } from "./screen.ts";

export { Reporter } from "./reporter.ts";
export type { ReproitEvent, Batch, ReporterConfig } from "./reporter.ts";
