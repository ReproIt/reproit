// See the package entry point for the cross-version semantics compatibility
// rationale.
// ignore_for_file: deprecated_member_use

/// Flutter semantics -> canonical [RNode] capture.
///
/// Walks the live semantics tree and produces the canonical node tree the
/// structural signature hashes (see `src/signature.dart` and docs/signature.md).
/// Localized text is NEVER read into the tree; only roles, input types, icons,
/// and developer ids (from Keys) flow in, so the signature is locale-invariant.
///
/// The same role-mapping table is mirrored byte-for-byte in the explorer
/// generated explorer scaffold so the runner and the SDK compute the
/// SAME signature for the same screen.
library reproit_capture;

import 'package:flutter/semantics.dart';
import 'package:flutter/widgets.dart';

import 'signature.dart';

/// Map a Flutter [SemanticsData] to the canonical Role vocabulary, derived from
/// flags/actions only, NEVER from the (localized) label. Ordered most-specific
/// first. Returns a role from [kRoles] (anything else normalizes to `node` in
/// the descriptor).
///
/// A password is a `textfield` with `type=password` (the obscured-ness is a
/// TYPE refinement, not a role), matching how the golden vectors model inputs.
String roleFromSemantics(SemanticsData d) {
  bool f(SemanticsFlag x) => d.hasFlag(x);
  if (f(SemanticsFlag.isTextField)) return 'textfield';
  if (f(SemanticsFlag.hasToggledState)) return 'switch';
  if (f(SemanticsFlag.hasCheckedState)) {
    return f(SemanticsFlag.isInMutuallyExclusiveGroup) ? 'radio' : 'checkbox';
  }
  if (f(SemanticsFlag.isSlider)) return 'slider';
  if (f(SemanticsFlag.isHeader)) return 'header';
  if (f(SemanticsFlag.isLink)) return 'link';
  if (f(SemanticsFlag.isButton)) return 'button';
  if (f(SemanticsFlag.isImage)) return 'image';
  // A tappable with no stronger trait is treated as a button.
  if (d.hasAction(SemanticsAction.tap)) return 'button';
  return 'node';
}

/// The optional input-`type` refinement for a textfield node, derived from
/// flags. Only `password` is reliably distinguishable from the semantics tree
/// (obscured); `text` is the default for any other textfield so two text inputs
/// still differ from a password input in the descriptor. Returns null for
/// non-textfield roles.
String? inputTypeFromSemantics(SemanticsData d, String role) {
  if (role != 'textfield') return null;
  if (d.hasFlag(SemanticsFlag.isObscured)) return 'password';
  return 'text';
}

/// A stable developer id from a widget Key, or null. Only deterministic
/// [ValueKey]s are accepted (UniqueKey/GlobalKey are allocated fresh per build).
String? idFromKey(Key? k) {
  if (k is ValueKey<String>) return k.value;
  if (k is ValueKey<int>) return k.value.toString();
  if (k is ValueKey) return '${k.value}';
  return null;
}

/// The displayed VALUE of a value-bearing semantics node (docs/signature.md
/// "Value-state", Layer 2), or null when the node bears no value. Three Flutter
/// value-roles are detected from semantics flags only (never from chrome text):
///
///   * a text field (`isTextField`)        -> its entered text (`d.value`),
///   * a slider (`isSlider`)               -> its value (`d.value`),
///   * a live region (`isLiveRegion`)      -> its announced text: `d.value` if
///     present, else `d.label` (aria-live's Flutter equivalent; treated as a
///     status/output value-role).
///
/// Returns the raw value string (possibly empty, which classifies to EMPTY).
/// Chrome roles (buttons, headers, plain text) return null here, so rule 1's
/// chrome-text exclusion is preserved.
///
/// Obscured (password) fields return null: their value is NEVER read, not even
/// to derive a value-class, honoring docs/data-handling.md ("Password and hidden
/// fields ... are never read at all") and matching the Web/RN SDKs. The field's
/// structural node is still emitted (so the tree topology is unchanged); it just
/// contributes no value to the `V:` section.
String? valueFromSemantics(SemanticsData d) {
  if (d.hasFlag(SemanticsFlag.isObscured)) return null;
  if (d.hasFlag(SemanticsFlag.isTextField)) return d.value;
  if (d.hasFlag(SemanticsFlag.isSlider)) return d.value;
  if (d.hasFlag(SemanticsFlag.isLiveRegion)) {
    return d.value.trim().isNotEmpty ? d.value : d.label;
  }
  return null;
}

/// True when a value-bearing semantics node must carry the Layer 3
/// [RNode.valueNode] opt-in flag so the oracle treats it as value-bearing even
/// though its structural role is not in `kValueRoles`. A text field's role IS a
/// value-role, so it needs no flag; a slider's role (`slider`) and a live
/// region's structural role (often `node`/`button`/`text`) are NOT value-roles,
/// so they must be flagged to enter the `V:` section.
bool valueNodeFlagFor(SemanticsData d) =>
    !d.hasFlag(SemanticsFlag.isTextField) &&
    (d.hasFlag(SemanticsFlag.isSlider) ||
        d.hasFlag(SemanticsFlag.isLiveRegion));
