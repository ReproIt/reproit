part of '../reproit_explorer.dart';

// ===========================================================================
// CONTENT-BUG oracle (EXPLORE:CONTENTBUG) - deterministic, label-based.
//
// The Flutter twin of the web runner's content-bug oracle. A rendered semantics
// LABEL carrying a stringify/template artifact is broken CONTENT leaked to the
// screen. Four classes, each a pure substring/structure test over the label,
// never a pixel or timing read, so the same tree yields the same finding
// byte-for-byte on every observe and on replay:
//   - [object Object]   : an object coerced to a string label
//   - {{ ... }} / ${ }  : an unrendered template placeholder (binding never ran)
//   - undefined / null  : a missing value coerced into the label as a WHOLE word
//   - NaN               : a number computation that went non-finite
// The classifiers and their precedence are byte-identical to the web runner's
// reasonOf so a finding's `reason` matches cross-platform. We scan the semantics
// tree (the same tree EXPLORE:STATE signs), so each finding is addressed by a
// stable, locale-invariant key (the node's developer key when present, else
// `role:<role>#<idx>` in document order), never by the text itself. The `\b`-
// style guards require the artifact token to STAND ALONE, so ordinary prose that
// merely contains "null" ("Null Island", "Cancellation") is not flagged. Clean
// apps render none of these, so the control stays silent.

/// Classify a label string into a stable content-bug reason tag, or null. Fixed
/// precedence, first match wins (byte-identical to runners/web/runner.mjs
/// reasonOf), so a label carries at most one reason.
String? contentBugReason(String text) {
  if (text.isEmpty) return null;
  if (text.contains('[object Object]')) return 'object-object';
  if (RegExp(r'\{\{[^}]*\}\}').hasMatch(text) ||
      RegExp(r'\$\{[^}]*\}').hasMatch(text)) {
    return 'unrendered-template';
  }
  if (RegExp(r'(^|[\s:>(\[,])undefined($|[\s.,!?)\]<])').hasMatch(text)) {
    return 'undefined';
  }
  if (RegExp(r'(^|[\s:>(\[,])null($|[\s.,!?)\]<])').hasMatch(text)) {
    return 'null';
  }
  if (RegExp(r'(^|[\s:>(\[,])NaN($|[\s.,!?)\]<])').hasMatch(text)) {
    return 'nan';
  }
  return null;
}

/// Scan the live semantics tree for content-bug artifacts as a list of
/// (key, reason, text) items, sorted by key then reason so the marker is
/// byte-identical run to run. The key is the node's developer key when one is
/// paired to it (same role+document-order pairing snapshot() uses), else
/// `role:<role>#<idx>`. Returns [] when nothing is broken (no marker emitted).
// STUCK-KEYBOARD ground truth: the soft keyboard is up (non-zero bottom view
// inset) while no EditableText holds primary focus. Keyboard visible <=> an
// editable focused is a platform invariant, so a violation is deterministic
// and false-positive-free. On-device the inset is the real IME frame; in
// widget tests it is only non-zero when the harness simulates it, so this is
// silent (never fires) in environments with no keyboard concept.
bool detectStuckKeyboard(WidgetTester t) {
  if (t.view.viewInsets.bottom <= 0) return false;
  final focus = FocusManager.instance.primaryFocus;
  final ctx = focus?.context;
  // unfocus() parks focus on the enclosing SCOPE node: a scope holding
  // primary focus means no real node is focused, so with the IME up that IS
  // the bug (and the scope's subtree must NOT be searched for editables --
  // it spans the whole screen and would always suppress).
  if (focus == null || focus is FocusScopeNode || ctx == null) return true;
  if (ctx.widget is EditableText) return false;
  var editable = false;
  // The focus node usually sits ON the EditableText, but a custom field can
  // attach it to a wrapper: accept an EditableText ancestor or descendant.
  ctx.visitAncestorElements((el) {
    if (el.widget is EditableText) {
      editable = true;
      return false;
    }
    return true;
  });
  if (!editable && ctx is Element) {
    void walk(Element el) {
      if (editable) return;
      if (el.widget is EditableText) {
        editable = true;
        return;
      }
      el.visitChildren(walk);
    }

    ctx.visitChildren(walk);
  }
  return !editable;
}

List<Map<String, dynamic>> detectContentBugs(WidgetTester t) {
  final root = _semanticsRoot(t);
  if (root == null) return const [];
  // Pair developer ids to canonical-role nodes in document order, exactly like
  // snapshot(), so a content-bug finding shares the EXPLORE:STATE selector.
  final keyedIdsByRole = <String, List<String>>{};
  for (final kt in collectKeyedTappables()) {
    (keyedIdsByRole[kt.value] ??= <String>[]).add(keyValueOf(kt.key));
  }
  final perRoleId = <String, int>{};
  final out = <Map<String, dynamic>>[];
  final seen = <String>{};
  void walk(SemanticsNode node) {
    final data = node.getSemanticsData();
    if (!data.flagsCollection.isHidden) {
      final role = roleOf(data);
      final idx = perRoleId[role] ?? 0;
      perRoleId[role] = idx + 1;
      final roleIds = keyedIdsByRole[role];
      final id = (roleIds != null && idx < roleIds.length)
          ? roleIds[idx]
          : null;
      // Consider both the label and the displayed value: a broken binding can
      // surface in either. First non-null reason wins; label is checked first.
      final label = data.label.trim();
      final value = data.value.trim();
      String? reason = contentBugReason(label);
      var hit = label;
      if (reason == null) {
        reason = contentBugReason(value);
        hit = value;
      }
      if (reason != null) {
        final key = id != null ? 'key:$id' : 'role:${normalizeRole(role)}#$idx';
        final dedup = '$key|$reason';
        if (seen.add(dedup)) {
          final clipped = hit.length > 80 ? hit.substring(0, 80) : hit;
          out.add({'key': key, 'reason': reason, 'text': clipped});
        }
      }
    }
    node.visitChildren((c) {
      walk(c);
      return true;
    });
  }

  walk(root);
  out.sort((a, b) {
    final ka = a['key'] as String, kb = b['key'] as String;
    if (ka != kb) return ka.compareTo(kb);
    return (a['reason'] as String).compareTo(b['reason'] as String);
  });
  return out;
}

// ===========================================================================
// BLANK-SCREEN oracle (EXPLORE:BLANKSCREEN) - deterministic, structural.
//
// The Flutter twin of the web runner's blankScreenScan (runners/web/
// hygiene-oracles.mjs): the state rendered NOTHING (zero visible text labels,
// zero tappables, zero text fields, zero images) while the window has a
// non-zero size. The classic shape is a build that failed before rendering
// content: the frame is up, the tree is an empty shell, and the user sees a
// blank screen. FP guards, all deliberate: the caller runs this only after
// its settle, so a still-building frame never fires; a null semanticsOwner
// means we cannot SEE the tree, not that the screen is blank, so it never
// fires (skip, silent); and an image-only screen (a full-bleed hero, a
// canvas) is NOT blank, mirroring the web scan's media check. Returns one
// [{key:"root", w, h}] record naming the scanned root and the LOGICAL window
// size, or [] when any content is visible.
List<Map<String, dynamic>> detectBlankScreen(WidgetTester t) {
  final root = _semanticsRoot(t);
  if (root == null) return const []; // semantics unavailable: never fire
  final size = t.view.physicalSize;
  if (size.width <= 0 || size.height <= 0) return const [];
  var content = false;
  void walk(SemanticsNode node) {
    if (content) return;
    final data = node.getSemanticsData();
    if (!data.flagsCollection.isHidden) {
      final named =
          data.label.trim().isNotEmpty ||
          data.value.trim().isNotEmpty ||
          data.tooltip.trim().isNotEmpty;
      if (named ||
          data.hasAction(SemanticsAction.tap) ||
          data.flagsCollection.isTextField ||
          data.flagsCollection.isImage) {
        content = true;
        return;
      }
    }
    node.visitChildren((c) {
      walk(c);
      return !content;
    });
  }

  walk(root);
  if (content) return const [];
  final dpr = t.view.devicePixelRatio;
  return [
    {
      'key': 'root',
      'w': (size.width / dpr).round(),
      'h': (size.height / dpr).round(),
    },
  ];
}

// ===========================================================================
// SAFE-AREA oracle (EXPLORE:SAFEAREA) - deterministic, geometric.
//
// An interactive control whose hit rect intersects a device safe-area inset --
// the status bar / notch / Dynamic Island (top), the home indicator (bottom),
// or a landscape notch / rounded corner (left or right) -- so the control is
// drawn under system chrome or a display cutout and is obscured / hard to tap.
// Ground truth is the platform inset geometry: t.view.viewPadding is the
// device's safe-area inset in PHYSICAL px (viewPadding, not padding, so a raised
// software keyboard never shrinks it), and _globalRect gives the control's hit
// rect in the same physical space. Both are pure layout facts read from the
// semantics tree (the same tree EXPLORE:STATE signs), so the same tree yields
// the same finding byte-for-byte on every observe and on replay. FP guards, all
// deliberate: a device/test with ZERO insets on every edge (no notch, no test
// override) never fires; only a tap-action node counts; and an intrusion under
// 1 logical px is treated as flush-adjacent rounding, not a collision. Findings
// are addressed by the same stable key grammar as EXPLORE:STATE (developer key
// when paired, else role:<role>#<idx>), never by text. Each item is
// {key, edge, by}: the control, which inset it overlaps, and the overlap depth
// in LOGICAL px. Deduped by key|edge, capped at 20, sorted by key then edge so
// the marker is byte-identical run to run. Returns [] when no control sits in an
// inset (no marker emitted).
List<Map<String, dynamic>> detectSafeArea(WidgetTester t) {
  final root = _semanticsRoot(t);
  if (root == null) return const [];
  final size = t.view.physicalSize;
  if (size.width <= 0 || size.height <= 0) return const [];
  final vp = t.view.viewPadding; // safe-area inset, PHYSICAL px
  final insetTop = vp.top, insetBottom = vp.bottom;
  final insetLeft = vp.left, insetRight = vp.right;
  // No device insets at all (no notch/home-indicator, or a test that set none):
  // there is no safe area to collide with, so never fire.
  if (insetTop <= 0 && insetBottom <= 0 && insetLeft <= 0 && insetRight <= 0) {
    return const [];
  }
  final dpr = t.view.devicePixelRatio;
  // Pair developer ids to canonical-role nodes in document order, exactly like
  // snapshot(), so a finding shares the EXPLORE:STATE selector.
  final keyedIdsByRole = <String, List<String>>{};
  for (final kt in collectKeyedTappables()) {
    (keyedIdsByRole[kt.value] ??= <String>[]).add(keyValueOf(kt.key));
  }
  final perRoleId = <String, int>{};
  final out = <Map<String, dynamic>>[];
  final seen = <String>{};
  void add(String key, String edge, double overlapPhysical) {
    final by = (overlapPhysical / dpr); // physical -> logical px
    if (by <= 1.0) return; // flush-adjacent rounding, not a collision
    final dedup = '$key|$edge';
    if (seen.add(dedup) && out.length < 20) {
      out.add({'key': key, 'edge': edge, 'by': by.round()});
    }
  }

  void walk(SemanticsNode node) {
    final data = node.getSemanticsData();
    if (!data.flagsCollection.isHidden) {
      final role = roleOf(data);
      final idx = perRoleId[role] ?? 0;
      perRoleId[role] = idx + 1;
      final roleIds = keyedIdsByRole[role];
      final id = (roleIds != null && idx < roleIds.length)
          ? roleIds[idx]
          : null;
      if (data.hasAction(SemanticsAction.tap)) {
        final r = _globalRect(node); // physical px
        if (r.width > 0 && r.height > 0) {
          final key = id != null
              ? 'key:$id'
              : 'role:${normalizeRole(role)}#$idx';
          // Overlap depth against each inset band (physical px). A band is the
          // strip between the screen edge and the inset boundary.
          if (insetTop > 0) {
            add(
              key,
              'top',
              (r.bottom < insetTop ? r.bottom : insetTop) - r.top,
            );
          }
          if (insetBottom > 0) {
            final bandTop = size.height - insetBottom;
            add(key, 'bottom', r.bottom - (r.top > bandTop ? r.top : bandTop));
          }
          if (insetLeft > 0) {
            add(
              key,
              'left',
              (r.right < insetLeft ? r.right : insetLeft) - r.left,
            );
          }
          if (insetRight > 0) {
            final bandLeft = size.width - insetRight;
            add(
              key,
              'right',
              r.right - (r.left > bandLeft ? r.left : bandLeft),
            );
          }
        }
      }
    }
    node.visitChildren((c) {
      walk(c);
      return true;
    });
  }

  walk(root);
  out.sort((x, y) {
    final kx = x['key'] as String, ky = y['key'] as String;
    if (kx != ky) return kx.compareTo(ky);
    return (x['edge'] as String).compareTo(y['edge'] as String);
  });
  return out;
}

// ===========================================================================
// BROKEN-ASSET oracle (EXPLORE:BROKENASSET, tofu only) - deterministic.
//
// The native slice of the web runner's brokenAssetScan (runners/web/
// hygiene-oracles.mjs): a VISIBLE label containing U+FFFD, the replacement
// character an encoding failure renders as tofu. The img/font reasons stay
// web-only (Flutter has no DOM subresources to interrogate), so the native
// `reason` vocabulary is a strict subset of the web one and the Rust parser
// is untouched. Scans the semantics labels + values with the same walk and
// stable key grammar as detectContentBugs; a pure substring test, never a
// pixel or timing read, so the same tree yields the same finding
// byte-for-byte on replay. Clean text renders no U+FFFD, so the control
// stays silent.
List<Map<String, dynamic>> detectTofu(WidgetTester t) {
  final root = _semanticsRoot(t);
  if (root == null) return const [];
  // Pair developer ids to canonical-role nodes in document order, exactly like
  // snapshot(), so a finding shares the EXPLORE:STATE selector.
  final keyedIdsByRole = <String, List<String>>{};
  for (final kt in collectKeyedTappables()) {
    (keyedIdsByRole[kt.value] ??= <String>[]).add(keyValueOf(kt.key));
  }
  final perRoleId = <String, int>{};
  final out = <Map<String, dynamic>>[];
  final seen = <String>{};
  void walk(SemanticsNode node) {
    final data = node.getSemanticsData();
    if (!data.flagsCollection.isHidden) {
      final role = roleOf(data);
      final idx = perRoleId[role] ?? 0;
      perRoleId[role] = idx + 1;
      final roleIds = keyedIdsByRole[role];
      final id = (roleIds != null && idx < roleIds.length)
          ? roleIds[idx]
          : null;
      // A broken decode can surface in the label or the displayed value.
      final label = data.label.trim();
      final value = data.value.trim();
      final hit = label.contains('�')
          ? label
          : (value.contains('�') ? value : null);
      if (hit != null) {
        final key = id != null ? 'key:$id' : 'role:${normalizeRole(role)}#$idx';
        if (seen.add(key)) {
          final clipped = hit.length > 60 ? hit.substring(0, 60) : hit;
          out.add({'key': key, 'reason': 'tofu', 'detail': clipped});
        }
      }
    }
    node.visitChildren((c) {
      walk(c);
      return true;
    });
  }

  walk(root);
  out.sort((a, b) => (a['key'] as String).compareTo(b['key'] as String));
  return out;
}
