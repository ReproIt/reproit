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
export { installCausalFetch } from "./causal.ts";

/**
 * Declare a semantic auth input to reproit without rendering metadata into the
 * terminal. Call while the field is present. The runner de-duplicates by id.
 * In production (where REPROIT_INPUTS_FILE is absent) this is a no-op.
 */
export function authInput(
  purpose: "username" | "email" | "phone" | "password" | "new-password" | "otp" | "passkey" | "recovery-code",
  id: string,
): void {
  const proc = (globalThis as any).process;
  const path = proc?.env?.REPROIT_INPUTS_FILE;
  if (!path || !id || !/^[A-Za-z0-9_.-]+$/.test(id)) return;
  try {
    const fs = proc.getBuiltinModule?.("node:fs");
    fs?.appendFileSync(path, JSON.stringify({ sel: `key:${id}`, inputPurpose: purpose }) + "\n");
  } catch {
    // Instrumentation must never affect the application under test.
  }
}
