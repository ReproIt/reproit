// Dogfood for the BLANK-SCREEN / BROKEN-ASSET(tofu) oracles
// (Flutter scaffold :: detectBlankScreen,
// detectTofu). The detector bodies below are PARITY COPIES of the scaffold
// functions because those files are not part of the published SDK. If the
// scaffold logic changes, change these fixtures too. Validates both directions:
//   blank      : a screen with content is silent; an empty SizedBox screen
//                fires one {key:"root", w, h} record.
//   tofu       : a Text rendering U+FFFD fires reason "tofu"; clean text is
//                silent.
import 'dart:ui' show CheckedState, Tristate;

import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_test/flutter_test.dart';

// ---------------------------------------------------------------------------
// PARITY COPIES of the scaffold helpers the detectors depend on.
// ---------------------------------------------------------------------------

// PARITY COPY of Flutter scaffold::kRoles.
const List<String> kRoles = <String>[
  'screen',
  'header',
  'text',
  'button',
  'link',
  'textfield',
  'image',
  'icon',
  'list',
  'listitem',
  'tab',
  'switch',
  'checkbox',
  'radio',
  'slider',
  'menu',
  'menuitem',
  'dialog',
  'group',
  'node',
];

// PARITY COPY of Flutter scaffold::normalizeRole.
String normalizeRole(String role) => kRoles.contains(role) ? role : 'node';

// PARITY COPY of Flutter scaffold::roleOf.
String roleOf(SemanticsData data) {
  final flags = data.flagsCollection;
  if (flags.isTextField) return 'textfield';
  if (flags.isToggled != Tristate.none) return 'switch';
  if (flags.isChecked != CheckedState.none) {
    return flags.isInMutuallyExclusiveGroup ? 'radio' : 'checkbox';
  }
  if (flags.isSlider) return 'slider';
  if (flags.isHeader) return 'header';
  if (flags.isLink) return 'link';
  if (flags.isButton) return 'button';
  if (flags.isImage) return 'image';
  if (data.hasAction(SemanticsAction.tap)) return 'button';
  return 'node';
}

// PARITY COPY of Flutter scaffold::keyStringOf.
String? keyStringOf(Widget w) {
  final k = w.key;
  if (k is ValueKey<String>) return 's:${k.value}';
  if (k is ValueKey<int>) return 'i:${k.value}';
  if (k is ValueKey) return 'v:${k.value}';
  return null;
}

// PARITY COPY of Flutter scaffold::keyValueOf.
String keyValueOf(String ks) {
  if (ks.startsWith('s:') || ks.startsWith('i:') || ks.startsWith('v:')) {
    return ks.substring(2);
  }
  return ks;
}

// PARITY COPY of Flutter scaffold::_isOffstageSubtree.
bool _isOffstageSubtree(Widget w) {
  if (w is Offstage) return w.offstage;
  if (w is TickerMode) return !w.enabled;
  if (w is Visibility) return !w.visible && !w.maintainInteractivity;
  return false;
}

// PARITY COPY of Flutter scaffold::elementRole.
String? elementRole(Widget w) {
  final t = w.runtimeType.toString();
  if (t.contains('EditableText') ||
      t.contains('TextField') ||
      t.contains('TextFormField') ||
      t.contains('CupertinoTextField')) {
    return 'textfield';
  }
  if (t.contains('Switch')) return 'switch';
  if (t.contains('Radio')) return 'radio';
  if (t.contains('Checkbox')) return 'checkbox';
  if (t.contains('Slider')) return 'slider';
  if (t.contains('Button') || t.contains('Chip') || t.contains('Tab')) {
    return 'button';
  }
  if (t.contains('InkWell') ||
      t.contains('GestureDetector') ||
      t.contains('InkResponse') ||
      t.contains('ListTile')) {
    return 'button';
  }
  if (t.contains('Image')) return 'image';
  return null;
}

// PARITY COPY of Flutter scaffold::collectKeyedTappables.
List<MapEntry<String, String>> collectKeyedTappables() {
  final out = <MapEntry<String, String>>[];
  void walk(Element e) {
    if (_isOffstageSubtree(e.widget)) return;
    final ks = keyStringOf(e.widget);
    final role = elementRole(e.widget);
    if (ks != null && role != null) out.add(MapEntry(ks, role));
    e.visitChildren(walk);
  }

  final root = WidgetsBinding.instance.rootElement;
  if (root != null) root.visitChildren(walk);
  return out;
}

SemanticsNode? rootSemanticsNode(WidgetTester tester) {
  for (final renderView in RendererBinding.instance.renderViews) {
    if (renderView.flutterView.viewId == tester.view.viewId) {
      return renderView.owner?.semanticsOwner?.rootSemanticsNode;
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// PARITY COPIES of the detectors under test.
// ---------------------------------------------------------------------------

// PARITY COPY of Flutter scaffold::detectBlankScreen.
List<Map<String, dynamic>> detectBlankScreen(WidgetTester t) {
  final root = rootSemanticsNode(t);
  if (root == null) return const []; // semantics unavailable: never fire
  final size = t.view.physicalSize;
  if (size.width <= 0 || size.height <= 0) return const [];
  var content = false;
  void walk(SemanticsNode node) {
    if (content) return;
    final data = node.getSemanticsData();
    if (!data.flagsCollection.isHidden) {
      final named = data.label.trim().isNotEmpty ||
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

// PARITY COPY of Flutter scaffold::detectTofu.
List<Map<String, dynamic>> detectTofu(WidgetTester t) {
  final root = rootSemanticsNode(t);
  if (root == null) return const [];
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
      final id =
          (roleIds != null && idx < roleIds.length) ? roleIds[idx] : null;
      final label = data.label.trim();
      final value = data.value.trim();
      final hit =
          label.contains('�') ? label : (value.contains('�') ? value : null);
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

// ---------------------------------------------------------------------------
// Fixtures.
// ---------------------------------------------------------------------------

void main() {
  // ---- blank-screen --------------------------------------------------------

  testWidgets('a screen with content is blank-silent', (t) async {
    final semantics = t.ensureSemantics();
    await t.pumpWidget(const MaterialApp(
      home: Scaffold(body: Center(child: Text('hello'))),
    ));
    expect(detectBlankScreen(t), isEmpty,
        reason: 'ANY visible content suppresses the WSOD oracle');
    semantics.dispose();
  });

  testWidgets('an empty SizedBox screen fires blank', (t) async {
    final semantics = t.ensureSemantics();
    await t.pumpWidget(const SizedBox());
    final items = detectBlankScreen(t);
    expect(items, hasLength(1));
    expect(items[0]['key'], 'root');
    expect(items[0]['w'], 800,
        reason: 'LOGICAL window size (800x600 test view)');
    expect(items[0]['h'], 600);
    semantics.dispose();
  });

  // NOTE: the null-semanticsOwner guard (skip, never fire) cannot be exercised
  // under current flutter_test: the test binding maintains a semantics owner
  // even without ensureSemantics, so there is no way to present a null owner
  // to the detector from a widget test. The guard matters on-device, where
  // semantics may genuinely be off.

  // ---- broken-asset (tofu) -------------------------------------------------

  testWidgets('a rendered U+FFFD fires tofu', (t) async {
    final semantics = t.ensureSemantics();
    await t.pumpWidget(const MaterialApp(
      home: Scaffold(body: Center(child: Text('glyph � here'))),
    ));
    final items = detectTofu(t);
    expect(items, hasLength(1));
    expect(items[0]['reason'], 'tofu');
    expect(items[0]['detail'], contains('�'));
    semantics.dispose();
  });

  testWidgets('clean text is tofu-silent', (t) async {
    final semantics = t.ensureSemantics();
    await t.pumpWidget(const MaterialApp(
      home: Scaffold(body: Center(child: Text('all glyphs resolve'))),
    ));
    expect(detectTofu(t), isEmpty);
    semantics.dispose();
  });
}
