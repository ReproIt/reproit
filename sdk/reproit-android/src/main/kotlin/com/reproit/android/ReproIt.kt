package com.reproit.android

import android.app.Activity
import android.app.Application
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.ViewTreeObserver
import java.io.OutputStream
import java.net.HttpURLConnection
import java.net.URL
import java.util.Locale
import java.util.TimeZone
import java.util.concurrent.Executors
import kotlin.random.Random

/**
 * ReproIt production telemetry for native Android (Kotlin).
 *
 * Emits the SAME state-graph and error events from real users that the reproit
 * test runners emit, so the production graph aligns 1:1 with test-time graphs and
 * a prod "cannot reproduce" becomes a deterministic replay. The signature and
 * payload shapes are byte-identical to `sdk/reproit-web.js`, the Flutter / iOS /
 * React-Native SDKs, and the runners.
 *
 * Usage (in your Application.onCreate):
 *
 * ```kotlin
 * class App : Application() {
 *     override fun onCreate() {
 *         super.onCreate()
 *         ReproIt.init(this, ReproItConfig(
 *             appId = "example",
 *             endpoint = "https://ingest.reproit.example",
 *             apiKey = "sk_...",
 *         ))
 *     }
 * }
 * ```
 *
 * The heavy logic (signature, snapshot reduction, batching, JSON) lives in the
 * pure-Kotlin [Engine] so it is host-testable; this class is the thin Android
 * binding (lifecycle, view-tree walk, taps, errors, HTTP).
 */
object ReproIt {
    /** Dependency-free HTTP client that captures/replays only during Reproit runs. */
    @JvmField
    val causalHttp = CausalHttp()
    private var engine: Engine? = null
    private var cfg: ReproItConfig? = null
    private var currentActivity: Activity? = null

    private val main = Handler(Looper.getMainLooper())
    private val io = Executors.newSingleThreadExecutor()
    private var snapshotRunnable: Runnable? = null
    private var flushRunnable: Runnable? = null
    private var layoutListener: ViewTreeObserver.OnGlobalLayoutListener? = null
    private var scrollListener: ViewTreeObserver.OnScrollChangedListener? = null
    private var firstObserved = false

    /** Optional developer-supplied screen anchor (the route/screen name). Set via
     * [screen]; mirrors `ReproItScreen("name")` / the Flutter SDK's anchor. When
     * set it prefixes the structural signature so two screens with the same shape
     * at different routes are distinct. */
    private var anchor: String? = null

    // Stable, namespaced tag keys for developer annotations on a View, read by
    // the structural capture. Using fixed integer keys (the View tag map accepts
    // any int) avoids needing generated R ids in this library.
    private const val R_ID_TEST_TAG = 0x7e_00_00_01
    private const val R_ID_ICON = 0x7e_00_00_02
    private const val R_ID_TRANSIENT = 0x7e_00_00_03
    private const val R_ID_VALUE_NODE = 0x7e_00_00_04

    /**
     * Set the current screen anchor (route / screen name). This becomes the `A:`
     * prefix of the structural signature, so a wizard's steps at one route, or
     * two same-shaped screens at different routes, hash distinctly. Pass null to
     * clear. Call it from your navigation layer (or annotate a screen view with
     * [tagScreen]).
     */
    @JvmStatic
    fun screen(name: String?) {
        anchor = name?.takeIf { it.isNotEmpty() }
    }

    /** Tag a view with a stable structural id (overrides the resource-id). */
    @JvmStatic
    fun tagId(view: View, id: String) {
        view.setTag(R_ID_TEST_TAG, id)
    }

    /** Tag a view with a language-independent icon identity. */
    @JvmStatic
    fun tagIcon(view: View, icon: String) {
        view.setTag(R_ID_ICON, icon)
    }

    /** Mark a view (and its subtree) transient so it is dropped from the hash. */
    @JvmStatic
    fun tagTransient(view: View) {
        view.setTag(R_ID_TRANSIENT, java.lang.Boolean.TRUE)
    }

    /**
     * Mark a view as value-bearing (Layer 3 opt-in, docs/signature.md
     * "Value-state"). Its displayed value (a `TextView`'s text, an `EditText`'s
     * entry, or the supplied [value]) is folded into the canonical signature as a
     * bounded, locale-safe value-class, even when the view's role is not in the
     * structural value-role set. Use this for counters / scores / stopwatches
     * shown in plain `TextView`s, where structure never moves but the value does.
     * Pass an explicit [value] to override what the capture reads from the view.
     */
    @JvmStatic
    @JvmOverloads
    fun tagValue(view: View, value: String? = null) {
        view.setTag(R_ID_VALUE_NODE, value ?: java.lang.Boolean.TRUE)
    }

    /** The current screen anchor used as the signature prefix. Currently the
     * developer-supplied anchor; route auto-detection can layer on later without
     * changing the signature contract. */
    private fun anchorOf(): String? = anchor

    /**
     * Zero-config start: the one-line quickstart. Begins telemetry with sensible
     * defaults and no required configuration, then delegates to [init]. Enabled
     * only for a debuggable build (the manifest `android:debuggable` flag, which
     * release builds clear); a no-op otherwise, so shipping this one line does
     * nothing in a release build. The app id is derived from the application
     * package name. To run in a release build, or to override any field, call
     * [init] with an explicit [ReproItConfig].
     */
    @JvmStatic
    fun start(application: Application) {
        val debuggable =
            (application.applicationInfo.flags and
                android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0
        if (!debuggable) return
        init(application, ReproItConfig(appId = application.packageName))
    }

    /** Initialize telemetry. Safe to call once; later calls are ignored. */
    @JvmStatic
    fun init(application: Application, config: ReproItConfig) {
        if (engine != null) return
        // Sampling decision, made once per session.
        if (config.sampleRate < 1.0 && Random.nextDouble() > config.sampleRate) return

        cfg = config
        engine = Engine(
            cfg = config,
            now = { System.currentTimeMillis() },
            transport = { body -> post(config, body) },
            log = { msg -> android.util.Log.d("reproit", msg) },
        )

        // Tier-1 auto dimensions: zero-PII, high-signal for "works for me but not
        // for them" bugs (platform, OS version, locale, timezone). These mirror
        // the Flutter SDK's init-time dimensions; the merge/serialization lives in
        // the pure-Kotlin Engine so it is host-testable.
        engine?.setContexts(
            mapOf(
                "platform" to "android",
                "os" to android.os.Build.VERSION.RELEASE,
                "locale" to Locale.getDefault().toLanguageTag(),
                "tz" to TimeZone.getDefault().id,
            )
        )

        application.registerActivityLifecycleCallbacks(Lifecycle)
        installErrorHandler()
        scheduleFlush()
    }

    /** Flush queued events immediately (e.g. before a known teardown). */
    @JvmStatic
    fun flush() {
        engine?.let { e -> io.execute { e.flush() } }
    }

    /**
     * Attach a hashed user id (so the cloud can group "these N users hit it"
     * without storing identity) plus optional PII-safe context dimensions. The
     * raw [userId] is never stored or sent; only a SHA-256 hex prefix (`uid`).
     */
    @JvmStatic
    @JvmOverloads
    fun identify(userId: String, context: Map<String, Any?>? = null) {
        engine?.identify(userId, context)
    }

    /** Set a single PII-safe context dimension (e.g. role, plan, a count bucket). */
    @JvmStatic
    fun setContext(key: String, value: Any?) {
        engine?.setContext(key, value)
    }

    /** Merge several PII-safe context dimensions at once. */
    @JvmStatic
    fun setContexts(values: Map<String, Any?>) {
        engine?.setContexts(values)
    }

    /**
     * Register an app invariant: a predicate that must hold in EVERY visited state
     * (a running total never negative, the selected tab always highlighted). [test]
     * returns true when it holds; returning false or throwing marks it VIOLATED (a
     * thrown exception's message becomes the finding message). Registration is
     * idempotent by [id] and INERT in production: the predicate is stored but only
     * evaluated when the SDK detects it is running under the reproit fuzzer (see
     * [underFuzzer]), so this is zero-overhead until a run reproduces it. Under the
     * fuzzer, a violated invariant is logged as a `REPROIT_INVARIANT` marker on
     * logcat for the mobile runner to scrape. Mirrors the web SDK's
     * `ReproIt.invariant`.
     */
    @JvmStatic
    fun invariant(id: String, test: () -> Boolean) {
        engine?.registerInvariant(id, test)
    }

    /**
     * Whether this app is running under the reproit fuzzer. Android has no
     * `navigator.webdriver` equivalent, and UiAutomator2 (the app runs in its own
     * process, un-instrumented) has no app-env channel, so the runner signals fuzz
     * mode two ways: the `debug.reproit.fuzz` system property (settable over the
     * Appium `mobile: shell` path with `setprop`, readable by any app because
     * `debug.*` props are unprivileged), and the `REPROIT_FUZZ` process env var (for
     * local / manual runs). Either set to "1" arms invariant evaluation; otherwise
     * the registry is never evaluated (production is inert).
     */
    private fun underFuzzer(): Boolean {
        if (System.getenv("REPROIT_FUZZ") == "1") return true
        return try {
            val c = Class.forName("android.os.SystemProperties")
            val m = c.getMethod("get", String::class.java)
            (m.invoke(null, "debug.reproit.fuzz") as? String) == "1"
        } catch (_: Throwable) {
            false
        }
    }

    // ---- lifecycle: track the foreground Activity ---------------------------

    private val Lifecycle = object : Application.ActivityLifecycleCallbacks {
        override fun onActivityCreated(a: Activity, b: Bundle?) {}
        override fun onActivityStarted(a: Activity) {}
        override fun onActivityResumed(a: Activity) {
            currentActivity = a
            attach(a)
            scheduleSnapshot()
        }

        override fun onActivityPaused(a: Activity) {
            if (currentActivity === a) {
                detach(a)
                flush()
            }
        }

        override fun onActivityStopped(a: Activity) {}
        override fun onActivitySaveInstanceState(a: Activity, b: Bundle) {}
        override fun onActivityDestroyed(a: Activity) {
            if (currentActivity === a) currentActivity = null
        }
    }

    // ---- observe the live view tree -----------------------------------------

    private fun attach(activity: Activity) {
        val decor = activity.window?.decorView ?: return
        val vto = decor.viewTreeObserver

        // Debounced snapshot on layout + scroll (the UI "settling" signal).
        val ll = ViewTreeObserver.OnGlobalLayoutListener { scheduleSnapshot() }
        val sl = ViewTreeObserver.OnScrollChangedListener { scheduleSnapshot() }
        layoutListener = ll
        scrollListener = sl
        if (vto.isAlive) {
            vto.addOnGlobalLayoutListener(ll)
            vto.addOnScrollChangedListener(sl)
        }

        // Tap capture: a pass-through touch listener records the down point and
        // hit-tests the tree for the clickable view under it. We do NOT consume
        // the event (return false), so the app's own handlers still run.
        decor.setOnTouchListener { _, ev ->
            if (ev.action == MotionEvent.ACTION_DOWN) {
                val target = tapTargetAt(decor, ev.rawX, ev.rawY)
                engine?.noteTap(target?.selector, target?.label)
            }
            false
        }
    }

    private fun detach(activity: Activity) {
        val decor = activity.window?.decorView ?: return
        val vto = decor.viewTreeObserver
        if (vto.isAlive) {
            layoutListener?.let { vto.removeOnGlobalLayoutListener(it) }
            scrollListener?.let { vto.removeOnScrollChangedListener(it) }
        }
        decor.setOnTouchListener(null)
    }

    private fun scheduleSnapshot() {
        val c = cfg ?: return
        snapshotRunnable?.let { main.removeCallbacks(it) }
        val r = Runnable { takeSnapshot() }
        snapshotRunnable = r
        main.postDelayed(r, c.debounceMs)
    }

    private fun scheduleFlush() {
        val c = cfg ?: return
        flushRunnable?.let { main.removeCallbacks(it) }
        val r = Runnable {
            flush()
            scheduleFlush()
        }
        flushRunnable = r
        main.postDelayed(r, c.flushMs)
    }

    private fun takeSnapshot() {
        val e = engine ?: return
        val decor = currentActivity?.window?.decorView ?: return
        val nodes = ArrayList<RawNode>()
        walk(decor, nodes)
        if (nodes.isEmpty() && firstObserved) return
        // Build the canonical STRUCTURAL node tree (role + id + type + icon +
        // shape; never localized text) and the screen anchor. The signature is
        // computed from these; `nodes` only supplies display-only labels.
        val tree = captureTree(decor)
        val anchor = anchorOf()
        val snap = e.reduce(nodes, tree, anchor)
        e.observe(snap, firstAction = "load")
        firstObserved = true
        // Self-triggered oracle: the native fuzzer drives this app and cannot call
        // the app's predicates, so the SDK evaluates its OWN registered invariants
        // on each settled state and logs a REPROIT_INVARIANT marker for the
        // violations (which the runner scrapes from logcat). Runs only under the
        // fuzzer; a no-op in production.
        if (underFuzzer()) {
            e.invariantMarker()?.let { android.util.Log.i("reproit", it) }
        }
    }

    /**
     * Recurse the view tree (pre-order: parent before children) collecting
     * visible nodes. A view contributes a name from contentDescription ?? text.
     * Used ONLY for the display-only label set, never for the hash.
     */
    private fun walk(view: View, out: MutableList<RawNode>) {
        if (view.visibility != View.VISIBLE) return
        if (view.width <= 0 || view.height <= 0) return

        val name = nameOf(view)
        if (name.isNotEmpty() || view.isClickable) {
            out.add(RawNode(name = name, tappable = view.isClickable))
        }
        if (view is ViewGroup) {
            for (i in 0 until view.childCount) walk(view.getChildAt(i), out)
        }
    }

    // ---- structural capture: View tree -> canonical Node tree ---------------
    //
    // Walks the live view tree into the canonical [Signature.Node] tree the
    // structural signature hashes (docs/signature.md "Inputs"). Roles are derived
    // from the widget class / a11y role, NEVER from the (localized) text. ids come
    // from the developer's resource-id or a `testTag` content-description marker;
    // input types from EditText `inputType`; icons are best-effort. The root is
    // forced to `screen`. The same role table is mirrored in the Flutter SDK
    // (`sdk/reproit_flutter/lib/src/capture.dart`) and the runners, so the SDK and
    // runner compute the SAME signature for the same screen.

    /** Build the canonical [Signature.Node] tree rooted at the decor view. */
    private fun captureTree(decor: View): Signature.Node {
        val children = ArrayList<Signature.Node>()
        if (decor is ViewGroup) {
            for (i in 0 until decor.childCount) {
                buildNode(decor.getChildAt(i))?.let { children.add(it) }
            }
        }
        return Signature.Node(role = "screen", children = children)
    }

    /** Map one visible view to a canonical node (with its visible subtree), or
     * null if the view is not visible. Invisible/zero-size wrappers are skipped
     * but their visible descendants are hoisted so structure is independent of
     * non-rendering wrappers. */
    private fun buildNode(view: View): Signature.Node? {
        if (view.visibility != View.VISIBLE) return null
        if (view.width <= 0 || view.height <= 0) return null

        val role = roleOf(view)
        val children = ArrayList<Signature.Node>()
        // Jetpack Compose: a `ComposeView` renders into an `AndroidComposeView`
        // whose internal composables are invisible to the View-tree walk (the
        // whole Compose UI would collapse to one opaque leaf, diverging from what
        // the runner sees). When this view IS that Compose root, walk its
        // SEMANTICS tree instead and splice the resulting canonical nodes in place
        // of its (opaque) View children, so the structural signature matches the
        // runner. `composeChildren` returns null for ordinary (non-Compose) views,
        // so the normal View recursion below still runs everywhere else.
        val composeChildren = composeChildrenOf(view)
        if (composeChildren != null) {
            children.addAll(composeChildren)
        } else if (view is ViewGroup) {
            for (i in 0 until view.childCount) {
                buildNode(view.getChildAt(i))?.let { children.add(it) }
            }
        }
        val value = valueOf(view, role)
        return Signature.Node(
            role = role,
            id = idOf(view),
            type = typeOf(view, role),
            icon = iconOf(view),
            transient = isTransientView(view),
            value = value,
            // The oracle only consults `valueNode` when a value is present; flag
            // every view that supplied a value but whose canonical role is not a
            // structural value-role (sliders, live-region status, opt-in plain
            // text), so the value-class enters the `V:` section. A `textfield`'s
            // role IS a value-role, so it needs no flag.
            valueNode = value != null && role != "textfield",
            children = children,
        )
    }

    /**
     * The displayed VALUE of a value-bearing view (docs/signature.md
     * "Value-state", Layer 2), or null when the view bears no value. Detected
     * from class / accessibility only, NEVER from chrome label text, so rule 1's
     * chrome-text exclusion holds. The sources, in order:
     *
     *   * an [tagValue] opt-in marker (Layer 3): the explicit string passed to
     *     [tagValue], else the view's own readable text;
     *   * an `EditText` (role `textfield`): its entered text;
     *   * an `AccessibilityNodeInfo` with a `RangeInfo` (sliders / progress /
     *     seekbars): the current range value;
     *   * a view with `accessibilityLiveRegion != NONE` (a status / announcing
     *     region): its current text (a status value-role).
     *
     * The returned string may be empty (which classifies to EMPTY). Chrome views
     * (buttons, headers, plain unmarked text) return null here.
     */
    private fun valueOf(view: View, role: String): String? {
        // Layer 3 opt-in marker. An explicit string wins; a bare TRUE marker
        // means "read this view's own value/text".
        val marker = view.getTag(R_ID_VALUE_NODE)
        if (marker is String) return marker
        if (marker == java.lang.Boolean.TRUE) {
            return readViewText(view)
        }

        // A text field's entered text (its role is the `textfield` value-role).
        if (view is android.widget.EditText) {
            return view.text?.toString() ?: ""
        }

        // A range (slider / progress / seekbar): read the current value from the
        // AccessibilityNodeInfo RangeInfo, where exposed.
        rangeValue(view)?.let { return it }

        // A live region announces status changes; treat its current text as a
        // status value-role.
        if (isLiveRegion(view)) {
            return readViewText(view)
        }
        return null
    }

    /** The current value of a view that exposes an accessibility `RangeInfo`
     * (a slider / progress / seekbar), formatted locale-independently, or null
     * when no range is present. */
    private fun rangeValue(view: View): String? {
        val info = try {
            android.view.accessibility.AccessibilityNodeInfo.obtain().also {
                view.onInitializeAccessibilityNodeInfo(it)
            }
        } catch (_: Throwable) {
            return null
        }
        try {
            val range = info.rangeInfo ?: return null
            val cur = range.current
            // Render integers without a trailing `.0` so a whole-number range
            // value classifies through the strict-decimal grammar (e.g. 5 ->
            // "5" -> POS1), and a fractional one keeps its period decimal.
            return if (cur == Math.floor(cur.toDouble()).toFloat() && !cur.isInfinite()) {
                cur.toLong().toString()
            } else {
                cur.toString()
            }
        } catch (_: Throwable) {
            return null
        } finally {
            @Suppress("DEPRECATION")
            info.recycle()
        }
    }

    /** True when a view is an accessibility live region (its content changes are
     * announced), i.e. `accessibilityLiveRegion != ACCESSIBILITY_LIVE_REGION_NONE`. */
    private fun isLiveRegion(view: View): Boolean = try {
        view.accessibilityLiveRegion != View.ACCESSIBILITY_LIVE_REGION_NONE
    } catch (_: Throwable) {
        false
    }

    /** The view's own readable value text (TextView text / EditText entry),
     * used for value capture only (live regions, opt-in markers). Returns an
     * empty string when there is none (EMPTY classifies deterministically). */
    private fun readViewText(view: View): String {
        if (view is android.widget.TextView) {
            return view.text?.toString() ?: ""
        }
        val cd = view.contentDescription?.toString()
        return cd ?: ""
    }

    /**
     * Map an Android [View] to the canonical Role vocabulary, from class / a11y
     * role only, never from text. Ordered most-specific first. Anything outside
     * the vocabulary normalizes to `node` in the descriptor.
     */
    private fun roleOf(view: View): String {
        // 1. Compose / explicit a11y role wins when it maps into the vocabulary.
        a11yRole(view)?.let { return it }
        // 2. Widget class.
        return when (view) {
            is android.widget.EditText -> "textfield"
            is android.widget.Switch -> "switch"
            is android.widget.RadioButton -> "radio"
            is android.widget.CheckBox -> "checkbox"
            is android.widget.SeekBar -> "slider"
            is android.widget.ProgressBar -> "progress" // transient (dropped)
            is android.widget.RatingBar -> "slider"
            is android.widget.ImageButton -> "button"
            is android.widget.Button -> "button"
            is android.widget.CompoundButton -> "checkbox"
            is android.widget.ImageView -> "image"
            is android.widget.ListView,
            is android.widget.GridView -> "list"
            is android.widget.TextView -> if (isHeader(view)) "header" else "text"
            is ViewGroup -> if (isRecyclerView(view)) "list" else "group"
            else -> "node"
        }
    }

    /**
     * Whether Jetpack Compose is on the runtime classpath. The Compose dependency
     * is `compileOnly`, so an app that does not ship Compose has no
     * `androidx.compose.ui.node.RootForTest` class; touching [ComposeCapture]
     * (which imports `androidx.compose.ui.*`) would then throw
     * `NoClassDefFoundError`. We probe once and skip Compose capture entirely when
     * it is absent, so the SDK adds zero requirements for non-Compose apps.
     */
    private val composePresent: Boolean by lazy {
        try {
            Class.forName(
                "androidx.compose.ui.node.RootForTest",
                false,
                ReproIt::class.java.classLoader,
            )
            true
        } catch (_: Throwable) {
            false
        }
    }

    /**
     * The canonical Compose-semantics children of [view] when it is a hosted
     * Compose root, else null (so the caller keeps walking the ordinary View
     * tree). Guarded by [composePresent] so it is a no-op when Compose is not on
     * the classpath; any failure degrades to null so a Compose-version mismatch
     * never crashes the host app.
     */
    private fun composeChildrenOf(view: View): List<Signature.Node>? {
        if (!composePresent) return null
        return try {
            ComposeCapture.composeChildren(view)
        } catch (_: Throwable) {
            null
        }
    }

    /** RecyclerView matched by class name to avoid a hard androidx dependency. */
    private fun isRecyclerView(view: View): Boolean {
        var c: Class<*>? = view.javaClass
        while (c != null) {
            if (c.simpleName == "RecyclerView") return true
            c = c.superclass
        }
        return false
    }

    /** Read a canonical role from the view's AccessibilityNodeInfo className /
     * roleDescription where one is exposed (covers Compose semantics, which do
     * not surface as concrete widget classes). Returns null when none maps. */
    private fun a11yRole(view: View): String? {
        val info = try {
            android.view.accessibility.AccessibilityNodeInfo.obtain().also {
                view.onInitializeAccessibilityNodeInfo(it)
            }
        } catch (_: Throwable) {
            return null
        }
        try {
            val cls = info.className?.toString() ?: ""
            val role = when {
                cls.endsWith("EditText") -> "textfield"
                cls.endsWith("Switch") || cls.endsWith("SwitchCompat") -> "switch"
                cls.endsWith("RadioButton") -> "radio"
                cls.endsWith("CheckBox") -> "checkbox"
                cls.endsWith("SeekBar") -> "slider"
                cls.endsWith("Button") -> "button"
                cls.endsWith("ImageView") -> "image"
                cls.endsWith("TabWidget") -> "tab"
                else -> {
                    if (info.isHeading) "header"
                    else if (info.isCheckable) "checkbox"
                    else if (info.isClickable) "button"
                    else null
                }
            }
            return role
        } finally {
            @Suppress("DEPRECATION")
            info.recycle()
        }
    }

    /** A header is a TextView the developer marked as a heading via
     * accessibility (`android:accessibilityHeading` / setAccessibilityHeading). */
    private fun isHeader(view: View): Boolean = try {
        view.isAccessibilityHeading
    } catch (_: Throwable) {
        false
    }

    /** The optional input-`type` refinement for a textfield node, from the
     * EditText `inputType`. `password` is distinguished; everything else is the
     * matching coarse type (email/number/text). Null for non-textfield roles. */
    private fun typeOf(view: View, role: String): String? {
        if (role != "textfield") return null
        if (view !is android.widget.EditText) return "text"
        val it = view.inputType
        val cls = it and android.text.InputType.TYPE_MASK_CLASS
        val variation = it and android.text.InputType.TYPE_MASK_VARIATION
        if (variation == android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD ||
            variation == android.text.InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD ||
            variation == android.text.InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD ||
            variation == android.text.InputType.TYPE_NUMBER_VARIATION_PASSWORD
        ) {
            return "password"
        }
        if (variation == android.text.InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS ||
            variation == android.text.InputType.TYPE_TEXT_VARIATION_WEB_EMAIL_ADDRESS
        ) {
            return "email"
        }
        if (cls == android.text.InputType.TYPE_CLASS_NUMBER) return "number"
        return "text"
    }

    /** A language-independent icon identity, if the developer attached one as a
     * stable tag (`R.id`-keyed) on the view. Never derived from text. Returns
     * null in the common case (Android has no portable icon-codepoint readout
     * from an arbitrary Drawable). */
    private fun iconOf(view: View): String? {
        val tag = view.getTag(R_ID_ICON) ?: return null
        val s = tag.toString().trim()
        return if (s.isEmpty()) null else s
    }

    /** Stable developer id: the view's resource-entry name (the `@+id/foo` the
     * developer wrote), or a `testTag` content-description marker. Synthetic
     * framework ids (no entry name) are omitted. */
    private fun idOf(view: View): String? {
        val id = view.id
        if (id != View.NO_ID) {
            try {
                val res = view.resources
                if (res != null) {
                    val entry = res.getResourceEntryName(id)
                    if (!entry.isNullOrBlank()) return entry
                }
            } catch (_: Throwable) {
            }
        }
        // `testTag`/explicit marker carried as a tag.
        val tag = view.getTag(R_ID_TEST_TAG)
        if (tag != null) {
            val s = tag.toString().trim()
            if (s.isNotEmpty()) return s
        }
        return null
    }

    /** Heuristic transient detection (rule 2): progress/spinner widgets and
     * views the developer flagged transient via a tag. Snackbars/toasts live in
     * their own windows and rarely appear in the decor tree, but Snackbar's
     * content view class is matched defensively. */
    private fun isTransientView(view: View): Boolean {
        if (view is android.widget.ProgressBar) return true
        val cls = view.javaClass.name
        if (cls.contains("Snackbar") || cls.contains("Tooltip") ||
            cls.contains("Toast")
        ) {
            return true
        }
        return view.getTag(R_ID_TRANSIENT) == java.lang.Boolean.TRUE
    }

    private fun nameOf(view: View): String {
        val cd = view.contentDescription?.toString()
        if (!cd.isNullOrBlank()) return cd
        if (view is android.widget.TextView) {
            val t = view.text?.toString()
            if (!t.isNullOrBlank()) return t
        }
        return ""
    }

    /**
     * The accessible name of the deepest clickable, named view under a screen
     * point. Children are visited after parents, so the deepest match wins.
     */
    private data class TapTarget(val selector: String, val label: String?)

    private fun tapTargetAt(root: View, rawX: Float, rawY: Float): TapTarget? {
        var best: TapTarget? = null
        val perRole = LinkedHashMap<String, Int>()
        fun visit(view: View) {
            if (view.visibility != View.VISIBLE) return
            val loc = IntArray(2)
            view.getLocationOnScreen(loc)
            val inX = rawX >= loc[0] && rawX <= loc[0] + view.width
            val inY = rawY >= loc[1] && rawY <= loc[1] + view.height
            val role = roleOf(view)
            val idx = if (view.isClickable) {
                val next = perRole[role] ?: 0
                perRole[role] = next + 1
                next
            } else {
                -1
            }
            if (inX && inY && view.isClickable) {
                val n = nameOf(view)
                val clean = engine?.cleanLabel(n)
                val selector = idOf(view)?.let { "key:$it" } ?: "role:$role#$idx"
                best = TapTarget(selector, clean)
            }
            if (view is ViewGroup) {
                for (i in 0 until view.childCount) visit(view.getChildAt(i))
            }
        }
        visit(root)
        return best
    }

    // ---- error oracle -------------------------------------------------------

    /**
     * Collect PII-safe fingerprints of on-screen text fields for the on-error
     * context. Walks the foreground decor view for `EditText`, fingerprints each
     * value to FEATURES, then discards the value. Raw text never escapes.
     *
     * HONEST LIMITATION (see README): password fields (an `inputType` with the
     * `TYPE_TEXT_VARIATION_PASSWORD` / `TYPE_NUMBER_VARIATION_PASSWORD` /
     * `TYPE_TEXT_VARIATION_VISIBLE_PASSWORD` flags) are skipped entirely; their
     * values are never read. Empty fields report `isEmpty:true`.
     */
    private fun collectFieldFingerprints(): List<Map<String, Any>> {
        val decor = currentActivity?.window?.decorView ?: return emptyList()
        val fields = ArrayList<Pair<String, String>>()
        var index = 0
        fun isPassword(et: android.widget.EditText): Boolean {
            val it = et.inputType
            val variation = it and android.text.InputType.TYPE_MASK_VARIATION
            return variation == android.text.InputType.TYPE_TEXT_VARIATION_PASSWORD ||
                variation == android.text.InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD ||
                variation == android.text.InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD ||
                variation == android.text.InputType.TYPE_NUMBER_VARIATION_PASSWORD
        }
        fun labelOf(et: android.widget.EditText): String {
            val cd = et.contentDescription?.toString()
            if (!cd.isNullOrBlank()) return cd
            val hint = et.hint?.toString()
            if (!hint.isNullOrBlank()) return hint
            return "#${index}".also { index++ }
        }
        fun visit(view: View) {
            if (view.visibility != View.VISIBLE) return
            if (view is android.widget.EditText) {
                if (!isPassword(view)) {
                    fields.add(labelOf(view) to (view.text?.toString() ?: ""))
                }
            }
            if (view is ViewGroup) {
                for (i in 0 until view.childCount) visit(view.getChildAt(i))
            }
        }
        visit(decor)
        return Fingerprint.fingerprintFields(fields)
    }

    private fun installErrorHandler() {
        val prior = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, throwable ->
            try {
                val stack = throwable.stackTrace.take(8).map { it.toString() }
                val top = throwable.stackTrace.firstOrNull()
                // Tier-3 on-error context: PII-safe input fingerprints.
                val fp = try { collectFieldFingerprints() } catch (_: Throwable) { emptyList() }
                val context = if (fp.isNotEmpty()) {
                    mapOf("fingerprint" to fp, "fpVersion" to Fingerprint.FP_VERSION)
                } else {
                    null
                }
                engine?.recordError(
                    message = throwable.toString(),
                    stack = stack,
                    source = top?.fileName ?: "",
                    line = top?.lineNumber ?: 0,
                    context = context,
                )
                // Flush synchronously on this thread before the process dies; the
                // io executor may not get a chance to run during a crash.
                engine?.flush()
            } catch (_: Throwable) {
            }
            prior?.uncaughtException(thread, throwable)
        }
    }

    // ---- transport ----------------------------------------------------------

    private fun post(config: ReproItConfig, body: String): Boolean {
        val endpoint = config.endpoint ?: return true
        return try {
            val url = URL("$endpoint/v1/events")
            val conn = url.openConnection() as HttpURLConnection
            conn.requestMethod = "POST"
            conn.connectTimeout = 8000
            conn.readTimeout = 8000
            conn.doOutput = true
            conn.setRequestProperty("Content-Type", "application/json")
            config.apiKey?.let { conn.setRequestProperty("Authorization", "Bearer $it") }
            val bytes = body.toByteArray(Charsets.UTF_8)
            val os: OutputStream = conn.outputStream
            os.use { it.write(bytes) }
            val code = conn.responseCode
            conn.disconnect()
            code in 200..299
        } catch (_: Throwable) {
            false
        }
    }
}
