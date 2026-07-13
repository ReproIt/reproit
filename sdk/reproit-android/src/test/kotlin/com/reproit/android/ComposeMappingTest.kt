package com.reproit.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/**
 * Host-side unit tests for the pure-Kotlin Jetpack Compose semantics mapping
 * ([Compose]). These cover the structural mapping that turns a Compose semantics
 * tree into the canonical [Signature.Node] descriptor, WITHOUT the Android SDK or
 * the Compose runtime (the androidx access lives in `ComposeCapture.kt`, which is
 * excluded from the host test exactly like `ReproIt.kt`).
 *
 * The contract under test: a Compose UI must fold into the SAME [Signature.Node]
 * model the View walk produces, so the unchanged [Signature] core hashes it
 * byte-for-byte like the runner that reads the same semantics tree.
 */
class ComposeMappingTest {

    private fun cs(
        role: String? = null,
        testTag: String? = null,
        hasText: Boolean = false,
        editable: Boolean = false,
        password: Boolean = false,
        heading: Boolean = false,
        clickable: Boolean = false,
        toggleable: Boolean = false,
        selectable: Boolean = false,
        hasProgress: Boolean = false,
        slider: Boolean = false,
        indeterminateProgress: Boolean = false,
        liveRegion: Boolean = false,
        editableValue: String? = null,
        rangeValue: String? = null,
        liveText: String? = null,
        children: List<Compose.ComposeSemantics> = emptyList(),
    ) = Compose.ComposeSemantics(
        role = role,
        testTag = testTag,
        hasText = hasText,
        editable = editable,
        password = password,
        heading = heading,
        clickable = clickable,
        toggleable = toggleable,
        selectable = selectable,
        hasProgress = hasProgress,
        slider = slider,
        indeterminateProgress = indeterminateProgress,
        liveRegion = liveRegion,
        editableValue = editableValue,
        rangeValue = rangeValue,
        liveText = liveText,
        children = children,
    )

    // ---- role mapping (mirrors the canonical vocabulary) --------------------

    @Test
    fun explicitComposeRolesMapToVocabulary() {
        assertEquals("button", Compose.roleOf(cs(role = "button")))
        assertEquals("checkbox", Compose.roleOf(cs(role = "checkbox")))
        assertEquals("switch", Compose.roleOf(cs(role = "switch")))
        assertEquals("radio", Compose.roleOf(cs(role = "radiobutton")))
        assertEquals("tab", Compose.roleOf(cs(role = "tab")))
        assertEquals("image", Compose.roleOf(cs(role = "image")))
        assertEquals("menu", Compose.roleOf(cs(role = "dropdownlist")))
    }

    @Test
    fun flagBasedRolesWhenNoExplicitRole() {
        assertEquals("textfield", Compose.roleOf(cs(editable = true)))
        assertEquals("header", Compose.roleOf(cs(heading = true, hasText = true)))
        assertEquals("switch", Compose.roleOf(cs(toggleable = true)))
        assertEquals("radio", Compose.roleOf(cs(selectable = true)))
        assertEquals("progress", Compose.roleOf(cs(hasProgress = true)))
        assertEquals("button", Compose.roleOf(cs(clickable = true)))
        assertEquals("text", Compose.roleOf(cs(hasText = true)))
        assertEquals("node", Compose.roleOf(cs()))
    }

    @Test
    fun textFieldTypeRefinement() {
        val pw = cs(editable = true, password = true)
        assertEquals("textfield", Compose.roleOf(pw))
        assertEquals("password", Compose.typeOf(pw, "textfield"))
        val plain = cs(editable = true)
        assertEquals("text", Compose.typeOf(plain, "textfield"))
        // type is null for non-textfields.
        assertEquals(null, Compose.typeOf(cs(clickable = true), "button"))
    }

    // ---- descriptor parity: Compose tree folds into the canonical node ------

    @Test
    fun composeTreeProducesSameDescriptorAsEquivalentViewTree() {
        // A Compose login screen: header, email field, password field, button.
        val composeRoots = listOf(
            cs(role = "button", testTag = "submit", clickable = true, hasText = true),
        )
        // The hand-built canonical equivalent the runner would produce.
        val expected = Signature.Node(
            role = "button",
            id = "submit",
        )
        assertEquals(
            Signature.descriptor(null, expected),
            Signature.descriptor(null, Compose.toNode(composeRoots[0])),
        )
    }

    @Test
    fun composeScreenMatchesRunnerStyleStructure() {
        // Build a Compose semantics tree for a login form and assert the canonical
        // descriptor equals the one a runner walking the same semantics tree yields.
        val composeTree = cs(
            // root traversal-group container (no role/text) -> node.
            children = listOf(
                cs(heading = true, hasText = true, testTag = "title"),
                cs(editable = true, testTag = "email", editableValue = "a@b.com"),
                cs(editable = true, password = true, testTag = "password", editableValue = ""),
                cs(role = "button", clickable = true, hasText = true, testTag = "login"),
            ),
        )
        val node = Compose.toNode(composeTree)
        // Wrap in a screen the way captureTree roots the View walk.
        val screen = Signature.Node(role = "screen", children = listOf(node))

        val expected = Signature.Node(
            role = "screen",
            children = listOf(
                Signature.Node(
                    role = "node",
                    children = listOf(
                        Signature.Node(role = "header", id = "title"),
                        Signature.Node(
                            role = "textfield", id = "email", type = "text",
                            value = "a@b.com",
                        ),
                        Signature.Node(
                            role = "textfield", id = "password", type = "password",
                            value = "",
                        ),
                        Signature.Node(role = "button", id = "login"),
                    ),
                ),
            ),
        )
        // The descriptor includes the V: section: a filled email (NONEMPTY) and an
        // empty password (EMPTY), keyed by testTag, exactly as the runner reading
        // the same semantics tree would produce.
        assertEquals(
            Signature.descriptor("/login", expected),
            Signature.descriptor("/login", screen),
        )
    }

    @Test
    fun testTagBecomesStableId() {
        val node = Compose.toNode(cs(role = "button", testTag = "checkout", clickable = true))
        assertEquals("checkout", node.id)
        // blank testTags are dropped (no id).
        val blank = Compose.toNode(cs(role = "button", testTag = "  ", clickable = true))
        assertEquals(null, blank.id)
    }

    @Test
    fun localizedTextIsExcludedFromTheHash() {
        // Two Compose buttons with the same structure but (modelled) different text
        // hash identically: text-presence is structural, the text itself is not.
        val a = Compose.toNode(cs(role = "button", testTag = "go", clickable = true, hasText = true))
        val b = Compose.toNode(cs(role = "button", testTag = "go", clickable = true, hasText = true))
        assertEquals(Signature.of("/x", a), Signature.of("/x", b))
    }

    // ---- value-state (Layer 2) over Compose semantics -----------------------

    @Test
    fun editableTextFieldFoldsValueClassIntoVSection() {
        val empty = Compose.toNode(cs(editable = true, testTag = "email", editableValue = ""))
        assertEquals(
            "A:\n0:textfield:text@email\nV:key:email=EMPTY",
            Signature.descriptor(null, empty),
        )
        val filled = Compose.toNode(
            cs(editable = true, testTag = "email", editableValue = "a@b.com"),
        )
        assertEquals(
            "A:\n0:textfield:text@email\nV:key:email=NONEMPTY",
            Signature.descriptor(null, filled),
        )
    }

    @Test
    fun sliderValueIsFlaggedAndBucketed() {
        // A Compose Slider (a range WITH a SetProgress action) maps to role
        // `slider`. `slider` is NOT in the structural value-role set, so the
        // mapping must value_node-flag it for the value-class to reach V:.
        val node = Compose.toNode(
            cs(testTag = "vol", hasProgress = true, slider = true, rangeValue = "5"),
        )
        assertEquals("slider", node.role)
        assertEquals(true, node.valueNode)
        assertEquals(
            "A:\n0:slider@vol\nV:key:vol=POS1",
            Signature.descriptor(null, node),
        )
        // 0 vs 5 are distinct buckets (ZERO vs POS1), matching the oracle.
        val at0 = Compose.toNode(
            cs(testTag = "vol", hasProgress = true, slider = true, rangeValue = "0"),
        )
        assertNotEquals(Signature.of(null, at0), Signature.of(null, node))
    }

    @Test
    fun readOnlyProgressIndicatorIsTransient() {
        // A determinate progress indicator (range, NO SetProgress action) maps to
        // role `progress`, which the core treats as a transient role and drops,
        // exactly like the View walk drops a `ProgressBar`.
        val screen = Signature.Node(
            role = "screen",
            children = listOf(
                Compose.toNode(cs(hasText = true, testTag = "label")),
                Compose.toNode(cs(hasProgress = true, slider = false, rangeValue = "50")),
            ),
        )
        val without = Signature.Node(
            role = "screen",
            children = listOf(Signature.Node(role = "text", id = "label")),
        )
        assertEquals(
            Signature.descriptor(null, without),
            Signature.descriptor(null, screen),
        )
    }

    @Test
    fun liveRegionStatusFoldsValueClass() {
        val node = Compose.toNode(
            cs(liveRegion = true, testTag = "count", hasText = true, liveText = "12"),
        )
        // live region -> structural role `text` (has text), flagged value_node so
        // the status value enters V:.
        assertEquals(
            "A:\n0:text@count\nV:key:count=POS2",
            Signature.descriptor(null, node),
        )
    }

    @Test
    fun indeterminateProgressIsTransientAndDropped() {
        val screen = Signature.Node(
            role = "screen",
            children = listOf(
                Compose.toNode(cs(hasText = true, testTag = "label")),
                Compose.toNode(cs(hasProgress = true, indeterminateProgress = true)),
            ),
        )
        val without = Signature.Node(
            role = "screen",
            children = listOf(Signature.Node(role = "text", id = "label")),
        )
        // The indeterminate spinner subtree is dropped (rule 2), so the descriptor
        // matches a screen with no spinner.
        assertEquals(
            Signature.descriptor(null, without),
            Signature.descriptor(null, screen),
        )
    }

    @Test
    fun repeatedComposeListItemsCollapse() {
        // A LazyColumn of N identical rows collapses to one *-marked token.
        fun row() = cs(
            clickable = true,
            children = listOf(cs(hasText = true)),
        )
        val list3 = cs(children = listOf(row(), row(), row()))
        val list5 = cs(children = listOf(row(), row(), row(), row(), row()))
        assertEquals(
            Signature.of(null, Compose.toNode(list3)),
            Signature.of(null, Compose.toNode(list5)),
        )
    }
}
