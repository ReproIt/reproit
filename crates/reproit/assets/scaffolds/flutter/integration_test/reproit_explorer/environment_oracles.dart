part of '../reproit_explorer.dart';

// ===========================================================================
// Capture determinism: signal the disable-animations preference for the whole
// run, the way the OS does (the binding's platformDispatcher accessibility-
// features override, the same test channel applyLocale uses, set BEFORE the app
// first pumps so MediaQuery.disableAnimations is true run-wide). This pins
// animation-dependent timing so snapshots are stable across runs, exactly like
// the web context's reducedMotion emulation.

/// Signal the disable-animations preference for the whole run, the way the OS
/// would. A binding without the test override leaves the app untouched.
bool applyReducedMotion(WidgetTester t) {
  try {
    t.binding.platformDispatcher.accessibilityFeaturesTestValue =
        const FakeAccessibilityFeatures(disableAnimations: true);
    return true;
  } catch (_) {
    return false;
  }
}

/// Clear the accessibility-features override so it is scoped to this run and
/// does not leak into a later test in the same process.
void clearReducedMotion(WidgetTester t) {
  try {
    t.binding.platformDispatcher.clearAccessibilityFeaturesTestValue();
  } catch (_) {}
}

// ===========================================================================
// PERMISSION-WALK oracle (EXPLORE:PERMISSIONWALK) - environment sweep.
//
// Under a permission-DENIAL sweep (REPROIT_DENY_PERMISSION set), mock the
// permission_handler platform channel so every runtime request answers
// permanentlyDenied, exactly as the OS would when the user taps "Don't allow"
// (and never asks again). We record that a denial happened and which permission
// it was; observe() then marks each screen reached AFTER that denial with
// EXPLORE:PERMISSIONWALK. The Rust invariant fires only for a marked screen that
// is ALSO trapped, attributing
// the trap to the denied permission. Outside a denial sweep this is inert (no
// mock installed, no marker emitted).
const String _permissionChannel = 'flutter.baseflow.com/permissions/methods';

/// True once the mocked permission channel has denied a request this run; gates
/// the EXPLORE:PERMISSIONWALK marker so only POST-denial screens are attributed.
bool permissionDenialSeen = false;

/// Install the deny-everything mock on the permission_handler channel. Returns
/// true iff the sweep is active (a non-empty REPROIT_DENY_PERMISSION), which is
/// the marker's gate. `checkPermissionStatus` reports denied; `requestPermissions`
/// reports permanentlyDenied for every requested permission AND latches
/// permissionDenialSeen (the denial event the attribution keys off).
bool installPermissionDenial(WidgetTester t, String perm) {
  if (perm.isEmpty) return false;
  permissionDenialSeen = false;
  const denied = 0; // PermissionStatus.denied
  const permanentlyDenied = 4; // PermissionStatus.permanentlyDenied
  t.binding.defaultBinaryMessenger.setMockMethodCallHandler(
    const MethodChannel(_permissionChannel),
    (MethodCall call) async {
      switch (call.method) {
        case 'requestPermissions':
          permissionDenialSeen = true;
          final args = call.arguments;
          final out = <int, int>{};
          if (args is List) {
            for (final p in args) {
              if (p is int) out[p] = permanentlyDenied;
            }
          }
          return out;
        case 'checkPermissionStatus':
          return denied;
        case 'checkServiceStatus':
          return 0;
        case 'shouldShowRequestPermissionRationale':
          return false;
        case 'openAppSettings':
          return false;
        default:
          return null;
      }
    },
  );
  return true;
}

/// Remove the permission mock so it is scoped to this run and does not leak into
/// a later test in the same process.
void clearPermissionDenial(WidgetTester t) {
  try {
    t.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      const MethodChannel(_permissionChannel),
      null,
    );
  } catch (_) {}
  permissionDenialSeen = false;
}

// ===========================================================================
// SCROLL ROUND-TRIP oracle (EXPLORE:SCROLLROUNDTRIP) - metamorphic, structural.
//
// The Flutter twin of the web runner's scrollRoundTripScan: the content at a
// pinned offset must be IDENTICAL after scrolling a list away and back. A list
// that recycles/rebinds a row without its data shows DIFFERENT content at the
// same position after the round-trip. The fingerprint is the normalized text of
// the RenderParagraphs in the TOP BAND of the primary scrollable's viewport,
// read before and after a jump to the end and back; pure-number tokens are
// normalized out so a clock/counter never counts as a mismatch. Self-restoring
// (the list is put back to its start offset). Async: the jumps need a pump.
// Returns [{pos, before, after}]; [] when the list is stable or nothing scrolls.
const double scrollRoundTripBandPx = 160.0;

Future<List<Map<String, dynamic>>> detectScrollRoundTrip(WidgetTester t) async {
  // Pick the largest scrollable that actually has hidden extent to test.
  ScrollableState? target;
  Rect? viewport;
  for (final el in find.byType(Scrollable).evaluate()) {
    if (el is! StatefulElement) continue;
    final state = el.state;
    if (state is! ScrollableState) continue;
    final pos = state.position;
    if (!pos.hasContentDimensions || pos.maxScrollExtent <= 200) continue;
    final ro = el.renderObject;
    if (ro is! RenderBox || !ro.hasSize) continue;
    final rect = ro.localToGlobal(Offset.zero) & ro.size;
    if (viewport == null ||
        rect.width * rect.height > viewport.width * viewport.height) {
      target = state;
      viewport = rect;
    }
  }
  if (target == null || viewport == null) return const [];

  String norm(String s) => s
      .replaceAll(RegExp(r'\d[\d.,:]*'), '#')
      .replaceAll(RegExp(r'\s+'), ' ')
      .trim();
  // The normalized text of the RenderParagraphs whose top is in the viewport's
  // top band, ordered top-to-bottom then left-to-right, first few joined.
  List<String> bandText(Rect band) {
    final root = WidgetsBinding.instance.rootElement?.renderObject;
    final hits = <MapEntry<Offset, String>>[];
    if (root == null) return const [];
    void walk(RenderObject ro) {
      if (ro is RenderParagraph && ro.hasSize) {
        final g = ro.localToGlobal(Offset.zero);
        if (g.dy >= band.top - 1 && g.dy < band.bottom) {
          final txt = norm(ro.text.toPlainText());
          if (txt.isNotEmpty) hits.add(MapEntry(g, txt));
        }
      }
      ro.visitChildren(walk);
    }

    walk(root);
    hits.sort((a, b) {
      final dy = a.key.dy.compareTo(b.key.dy);
      return dy != 0 ? dy : a.key.dx.compareTo(b.key.dx);
    });
    return hits.take(4).map((e) => e.value).toList();
  }

  final pos = target.position;
  final start = pos.pixels;
  final bandH = viewport.height < scrollRoundTripBandPx
      ? viewport.height
      : scrollRoundTripBandPx;
  final band = Rect.fromLTWH(
    viewport.left,
    viewport.top,
    viewport.width,
    bandH,
  );
  try {
    final before = bandText(band).join('|');
    pos.jumpTo(pos.maxScrollExtent);
    await t.pump(const Duration(milliseconds: 50));
    pos.jumpTo(0);
    await t.pump(const Duration(milliseconds: 50));
    final after = bandText(band).join('|');
    if (before.isEmpty || before == after) return const [];
    String clip(String s) => s.length > 120 ? s.substring(0, 120) : s;
    return [
      {
        'pos': 'y=${band.top.round()}',
        'before': clip(before),
        'after': clip(after),
      },
    ];
  } finally {
    try {
      pos.jumpTo(start);
      await t.pump(const Duration(milliseconds: 50));
    } catch (_) {}
  }
}
