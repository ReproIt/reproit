package com.reproit.android

/**
 * Pure-Kotlin Jetpack Compose semantics mapping.
 *
 * A Compose UI renders into a single native [android.view.View] (an
 * `AndroidComposeView` hosted by a `ComposeView`), so the classic View-tree walk
 * in [ReproIt] collapses the whole Compose subtree to one opaque leaf. That makes
 * the production signature DIVERGE from what the fuzz runner (Appium /
 * UiAutomator2) sees, because the runner reads the Compose *semantics* tree (the
 * same tree TalkBack reads): each composable surfaces as a node with a role, text,
 * content-description, testTag, and editable/value state.
 *
 * To close that gap [ReproIt] walks the Compose `SemanticsOwner` and turns every
 * semantics node into a [ComposeSemantics] holder, then folds it into the SAME
 * [Signature.Node] descriptor the View walk produces, so the canonical signature
 * core ([Signature]) is unchanged and stays parity-pinned to the Rust oracle and
 * every other SDK.
 *
 * This file is deliberately PURE Kotlin: it has NO `androidx.*` or `android.*`
 * imports. The Android layer ([ReproIt]) does the reflection-light androidx
 * semantics-tree access and builds [ComposeSemantics] holders; the structural
 * mapping ([toNode] / [roleOf] / [valueOf]) lives here so it is host-unit-testable
 * without the Android SDK or the Compose runtime, exactly like [Signature] and
 * [Engine]. The role/type/value rules mirror the Flutter SDK's semantics mapping
 * (`sdk/reproit_flutter/lib/src/capture.dart`) so a Compose screen and the runner
 * agree on the same canonical structure.
 */
object Compose {

    /**
     * A flattened, framework-free view of one Compose semantics node. The Android
     * layer reads each field from the real `SemanticsNode` / `SemanticsConfiguration`
     * (the `SemanticsProperties.*` keys) and never reads localized text into the
     * structural mapping; the text-presence flags below mark only that text EXISTS,
     * which is what distinguishes a `text` node from an empty container, the same
     * way the View walk treats a `TextView`. The actual strings are kept out of the
     * descriptor by rule 1 of docs/signature.md.
     */
    data class ComposeSemantics(
        /**
         * The semantic [Role] as a lowercase canonical name when Compose exposes one
         * (`SemanticsProperties.Role` -> Button/Checkbox/Switch/RadioButton/Tab/
         * Image/DropdownList/ValuePicker/Carousel), else null. Stored as a plain
         * string so this file needs no Compose import. Unknown names fall through to
         * the flag-based heuristics in [roleOf].
         */
        val role: String? = null,
        /** testTag (`SemanticsProperties.TestTag`), the stable developer id the
         * runner sees when `testTagsAsResourceId` is on. Highest-priority id. */
        val testTag: String? = null,
        /** True when the node carries `SemanticsProperties.Text` (display text). */
        val hasText: Boolean = false,
        /** True when the node carries `SemanticsProperties.ContentDescription`. */
        val hasContentDescription: Boolean = false,
        /** True when the node is an editable text field
         * (`SemanticsProperties.EditableText` present, or the text-input actions
         * `SetText`/`InsertTextAtCursor` are available). */
        val editable: Boolean = false,
        /** True when the node is a password field (`SemanticsProperties.Password`). */
        val password: Boolean = false,
        /** True when the node is marked a heading (`SemanticsProperties.Heading`). */
        val heading: Boolean = false,
        /** True when the node has a click action (`SemanticsActions.OnClick`). */
        val clickable: Boolean = false,
        /** True when the node is a toggle (`SemanticsProperties.ToggleableState`)
         * without an explicit switch/checkbox role. */
        val toggleable: Boolean = false,
        /** True when the node is selectable (`SemanticsProperties.Selected`). */
        val selectable: Boolean = false,
        /** True when the node carries a progress/range
         * (`SemanticsProperties.ProgressBarRangeInfo`). */
        val hasProgress: Boolean = false,
        /** True when the node is an adjustable slider (a range WITH a
         * `SemanticsActions.SetProgress` action), as opposed to a read-only
         * progress indicator. A Compose `Slider` has this action; a
         * `LinearProgressIndicator` does not. */
        val slider: Boolean = false,
        /** True when the node is an indeterminate progress indicator (a transient
         * spinner): a progress range whose value is the Indeterminate marker. */
        val indeterminateProgress: Boolean = false,
        /** True when the node is a live region (`SemanticsProperties.LiveRegion`). */
        val liveRegion: Boolean = false,
        /** The editable text field's current entered value (read from
         * `SemanticsProperties.EditableText`), used only for the Layer-2
         * value-class. Null for non-editable nodes. */
        val editableValue: String? = null,
        /** A progress/slider current value rendered locale-independently (from
         * `ProgressBarRangeInfo.current`), used only for the Layer-2 value-class.
         * Null when there is no range. */
        val rangeValue: String? = null,
        /** A live-region's current text (state-description / text), used only for
         * the Layer-2 status value-class. Null for non-live-region nodes. */
        val liveText: String? = null,
        /** Ordered child semantics nodes, in traversal order. */
        val children: List<ComposeSemantics> = emptyList(),
    )

    /**
     * Map a Compose [ComposeSemantics] node to the canonical Role vocabulary
     * (docs/signature.md "Roles"), from semantics only, never from text. Ordered
     * most-specific first, mirroring the Flutter SDK's `roleFromSemantics`. A text
     * field is `textfield` (its obscured/email/number refinement is a TYPE, set by
     * [typeOf], not a role). Anything outside the vocabulary normalizes to `node`
     * in the descriptor via [Signature.normalizeRole].
     */
    fun roleOf(n: ComposeSemantics): String {
        // 1. An explicit Compose Role wins when it maps into the vocabulary.
        when (n.role) {
            "button" -> return "button"
            "checkbox" -> return "checkbox"
            "switch" -> return "switch"
            "radiobutton", "radio" -> return "radio"
            "tab" -> return "tab"
            "image" -> return "image"
            "dropdownlist" -> return "menu"
            // ValuePicker / Carousel / others have no vocabulary role; fall through.
        }
        // 2. Flag-based heuristics (cover composables with no explicit Role).
        if (n.editable) return "textfield"
        if (n.heading) return "header"
        if (n.toggleable) return "switch"
        if (n.selectable) return "radio"
        if (n.slider) return "slider"
        if (n.hasProgress) return "progress" // read-only indicator; transient role
        if (n.clickable) return "button"
        if (n.hasText) return "text"
        return "node"
    }

    /**
     * The optional input-`type` refinement for a textfield node (docs/signature.md
     * "Inputs"), mirroring the Flutter SDK. Only `password` is reliably detectable
     * from Compose semantics; everything else is the coarse `text`. Null for
     * non-textfield roles.
     */
    fun typeOf(n: ComposeSemantics, role: String): String? {
        if (role != "textfield") return null
        if (n.password) return "password"
        return "text"
    }

    /**
     * True when this node must be dropped during normalization (rule 2): an
     * INDETERMINATE progress indicator (a spinner) is transient. A determinate
     * progress bar is NOT transient here (it carries a value-class); [Signature]
     * itself maps the `progress` role to a transient role, so a plain determinate
     * progress is dropped by the core unless it is value-bearing, exactly as the
     * View walk and the oracle treat `ProgressBar`. We only set the explicit
     * transient flag for the indeterminate spinner case so its subtree is dropped
     * regardless of role.
     */
    fun isTransient(n: ComposeSemantics): Boolean = n.indeterminateProgress

    /**
     * The displayed VALUE of a value-bearing Compose node (docs/signature.md
     * "Value-state", Layer 2), or null when it bears no value. Detected from
     * semantics only, never from chrome label text:
     *
     *   * an editable text field           -> its entered text ([editableValue]),
     *   * a slider / progress range        -> its current value ([rangeValue]),
     *   * a live region                    -> its current text ([liveText]).
     *
     * Chrome nodes (buttons, headers, plain text) return null. The returned string
     * may be empty (classifies to EMPTY).
     */
    fun valueOf(n: ComposeSemantics): String? {
        if (n.editable) return n.editableValue ?: ""
        if (n.rangeValue != null) return n.rangeValue
        if (n.liveRegion) return n.liveText ?: ""
        return null
    }

    /**
     * Whether the Layer-3 `valueNode` flag must be set so the value enters the
     * `V:` section. A `textfield`'s role IS a value-role, so it needs no flag; a
     * slider's role (`slider`) and a live region's structural role (`node`/`text`/
     * `button`) are NOT value-roles, so they must be flagged, mirroring the Flutter
     * SDK's `valueNodeFlagFor`.
     */
    private fun valueNodeFlag(n: ComposeSemantics, value: String?): Boolean =
        value != null && !n.editable

    /**
     * Fold one Compose semantics subtree into a canonical [Signature.Node],
     * recursively. This is the bridge: the result is hashed by the UNCHANGED
     * [Signature] core, so a Compose screen produces the same structural signature
     * the runner (which reads the same semantics tree) computes.
     *
     * The id precedence is testTag first (the stable developer id the runner reads
     * via `testTagsAsResourceId`), matching docs/signature.md's id source order.
     */
    fun toNode(n: ComposeSemantics): Signature.Node {
        val role = roleOf(n)
        val value = valueOf(n)
        return Signature.Node(
            role = role,
            id = n.testTag?.trim()?.takeIf { it.isNotEmpty() },
            type = typeOf(n, role),
            icon = null,
            transient = isTransient(n),
            value = value,
            valueNode = valueNodeFlag(n, value),
            children = n.children.map { toNode(it) },
        )
    }

    /**
     * Fold a list of root Compose semantics nodes (the children of a hosted
     * `ComposeView`) into a list of canonical child [Signature.Node]s, so the
     * caller can splice them in place of the opaque `ComposeView` leaf in the
     * surrounding View-tree node tree.
     */
    fun toNodes(roots: List<ComposeSemantics>): List<Signature.Node> =
        roots.map { toNode(it) }
}
