package com.reproit.android

import android.view.View
import androidx.compose.ui.node.RootForTest
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.semantics.SemanticsNode
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.semantics.getOrNull

/**
 * Android-side bridge from a hosted Jetpack Compose view to the pure-Kotlin [Compose] mapper.
 *
 * A `ComposeView` renders into an `AndroidComposeView` (the concrete
 * [androidx.compose.ui.node.RootForTest]) that exposes the `SemanticsOwner`, the SAME semantics
 * tree TalkBack and the Appium/UiAutomator2 runner read. We walk that tree, read each node's
 * `SemanticsProperties.*` reflection-free, and build a framework-free [Compose.ComposeSemantics]
 * holder; [Compose.toNodes] then folds those into canonical [Signature.Node]s so the unchanged
 * [Signature] core hashes them identically to what the runner sees.
 *
 * This file imports `androidx.compose.ui.*`, so (like [ReproIt]) it needs the Compose dependency
 * and is EXCLUDED from the host parity test. The Compose dependency is `compileOnly`: an app that
 * does not use Compose simply has no `RootForTest` on screen, so [extractRootForTest] returns null
 * and nothing here runs. The pure structural mapping it delegates to ([Compose]) IS host-tested.
 */
internal object ComposeCapture {

  /**
   * If [view] is a hosted Compose root (an `AndroidComposeView`, which implements [RootForTest]),
   * return the canonical [Signature.Node] children produced by walking its semantics tree, to be
   * spliced in place of the opaque view's children. Returns null when [view] is not a Compose root
   * (so the caller keeps recursing the ordinary View tree). Any failure degrades to null so a
   * Compose-version mismatch never crashes the host app.
   */
  fun composeChildren(view: View): List<Signature.Node>? {
    val root = extractRootForTest(view) ?: return null
    return try {
      // Use the UNMERGED tree so each composable surfaces as its own node,
      // matching what the runner enumerates (a merged tree would fold a
      // Button's text into the Button and hide structure).
      val rootNode = root.semanticsOwner.unmergedRootSemanticsNode
      rootNode.children.map { read(it) }.map { Compose.toNode(it) }
    } catch (_: Throwable) {
      null
    }
  }

  /**
   * Detect the Compose root that exposes the semantics owner. The concrete `AndroidComposeView`
   * implements [RootForTest]; we match by interface so we do not depend on the internal class name.
   */
  private fun extractRootForTest(view: View): RootForTest? = view as? RootForTest

  /**
   * Read one [SemanticsNode] into a framework-free [Compose.ComposeSemantics], recursively. All
   * reads are reflection-free through the public `SemanticsProperties` / `SemanticsActions` keys,
   * so they are stable across Compose versions that keep those public keys.
   */
  private fun read(node: SemanticsNode): Compose.ComposeSemantics {
    val cfg = node.config

    val role: String? = cfg.getOrNull(SemanticsProperties.Role)?.toString()?.lowercase()

    val editableText = cfg.getOrNull(SemanticsProperties.EditableText)?.text
    val hasSetText = cfg.getOrNull(SemanticsActions.SetText) != null
    val editable = editableText != null || hasSetText

    val password = cfg.contains(SemanticsProperties.Password)
    val heading = cfg.contains(SemanticsProperties.Heading)
    val clickable = cfg.getOrNull(SemanticsActions.OnClick) != null

    val toggleState = cfg.getOrNull(SemanticsProperties.ToggleableState)
    val toggleable = toggleState != null
    val selected = cfg.getOrNull(SemanticsProperties.Selected)
    val selectable = selected != null

    val range = cfg.getOrNull(SemanticsProperties.ProgressBarRangeInfo)
    val hasProgress = range != null
    // A Slider exposes a SetProgress action; a read-only progress indicator
    // does not. This is how we tell an adjustable slider from a progress bar.
    val isSlider = range != null && cfg.getOrNull(SemanticsActions.SetProgress) != null
    // ProgressBarRangeInfo.Indeterminate has an empty range (0f..0f). Compose
    // models an indeterminate spinner exactly that way; treat it as transient.
    val indeterminate = range != null && range.range.endInclusive <= range.range.start

    val liveRegion = cfg.contains(SemanticsProperties.LiveRegion)

    val hasText = cfg.getOrNull(SemanticsProperties.Text)?.isNotEmpty() == true
    val hasContentDescription =
      cfg.getOrNull(SemanticsProperties.ContentDescription)?.isNotEmpty() == true

    val testTag = cfg.getOrNull(SemanticsProperties.TestTag)

    val rangeValue: String? = range?.let { formatFloat(it.current) }

    val liveText: String? =
      if (liveRegion) {
        cfg.getOrNull(SemanticsProperties.StateDescription)
          ?: cfg.getOrNull(SemanticsProperties.Text)?.joinToString("") { it.text }
      } else {
        null
      }

    return Compose.ComposeSemantics(
      role = role,
      testTag = testTag,
      hasText = hasText,
      hasContentDescription = hasContentDescription,
      editable = editable,
      password = password,
      heading = heading,
      clickable = clickable,
      // A toggle with an explicit Switch/Checkbox role is already handled by
      // `role`; only flag the role-less toggle case so [Compose.roleOf] does
      // not double-map. Same for selectable vs an explicit RadioButton role.
      toggleable = toggleable && role == null,
      selectable = selectable && role == null,
      hasProgress = hasProgress,
      slider = isSlider,
      indeterminateProgress = indeterminate,
      liveRegion = liveRegion,
      editableValue = editableText,
      rangeValue = rangeValue,
      liveText = liveText,
      children = node.children.map { read(it) },
    )
  }

  /**
   * Render a slider/progress value locale-independently: a whole number with no trailing `.0` (so
   * `5f` -> "5" -> POS1 through the strict-decimal grammar), a fractional one with its period
   * decimal.
   */
  private fun formatFloat(v: Float): String {
    if (v.isNaN() || v.isInfinite()) return v.toString()
    return if (v == kotlin.math.floor(v)) v.toLong().toString() else v.toString()
  }
}
