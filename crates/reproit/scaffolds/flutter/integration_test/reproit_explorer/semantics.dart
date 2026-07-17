part of '../reproit_explorer.dart';

List<Map<String, dynamic>> roleElements(Snapshot snap) => snap.tappables
    .take(maxLabelsPerState)
    .map((element) => {"role": element.role})
    .toList();

List<Map<String, dynamic>> stateElements(Snapshot snap) => snap.tappables
    .take(maxLabelsPerState)
    .map(
      (element) => {
        "sel": element.sel,
        "role": element.role,
        "label": element.label,
        if (element.inputPurpose != null) "inputPurpose": element.inputPurpose,
        if (!element.hasKey) "nokey": true,
      },
    )
    .toList();

/// Map a Flutter [SemanticsData] to the canonical Role vocabulary from
/// flags/actions only, NEVER from the (localized) label. A password is a
/// `textfield` with `type=password` (a TYPE refinement, not a role).
String roleOf(SemanticsData data) {
  final flags = data.flagsCollection;
  if (flags.isTextField) return 'textfield';
  if (flags.isToggled != ui.Tristate.none) return 'switch';
  if (flags.isChecked != ui.CheckedState.none) {
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

/// The optional input-`type` refinement for a textfield node, from flags only.
String? inputTypeOf(SemanticsData data, String role) {
  if (role != 'textfield') return null;
  return data.flagsCollection.isObscured ? 'password' : 'text';
}

/// The displayed VALUE of a value-bearing semantics node (Layer 2), or null.
/// Detected from flags only: a text field's entered text (`d.value`), a slider's
/// value (`d.value`), and a live region (aria-live's Flutter equivalent: its
/// `d.value` if set, else `d.label`, treated as a status value-role). Chrome
/// roles return null so rule 1's chrome-text exclusion is preserved.
String? valueOf(SemanticsData data) {
  if (data.flagsCollection.isTextField) return data.value;
  if (data.flagsCollection.isSlider) return data.value;
  if (data.flagsCollection.isLiveRegion) {
    return data.value.trim().isNotEmpty ? data.value : data.label;
  }
  return null;
}

/// True when a value-bearing node needs the Layer 3 `valueNode` flag because its
/// structural role is NOT a value-role: a slider (role `slider`) and a live
/// region (often `node`/`text`/`button`). A text field's role IS a value-role,
/// so it needs no flag.
bool valueNodeFlagOf(SemanticsData data) =>
    !data.flagsCollection.isTextField &&
    (data.flagsCollection.isSlider || data.flagsCollection.isLiveRegion);

/// The screen anchor (route template / screen-level key). Captured from the top
/// route's name; a ReproItScreen marker or screen-level Key would also feed here
/// if present. Null/empty leaves the anchor empty (the A: line is still emitted).
///
/// DEEP-LINK PARITY is EXCLUDED on Flutter (ground truth, not effort). That
/// oracle reopens each visited route's URL COLD and diffs the structure, so it
/// needs (a) an addressable per-route identity and (b) a way to cold-boot the app
/// at that route. Neither is generically available here: the app-integration
/// point (`pumpApp`) pumps ONE root widget with no route-parameterized cold-boot
/// entry, and the fuzzer reaches screens by tapping, which pushes anonymous
/// `MaterialPageRoute`s whose `settings.name` is null -- there is no URL to
/// derive and re-open. (An app using a URL-based Router could expose one, but the
/// explorer cannot assume it.) Web, where the address bar IS the route, is where
/// this oracle applies.
String? screenAnchor(WidgetTester t) {
  try {
    String? name;
    final nav = t.state<NavigatorState>(find.byType(Navigator).first);
    nav.popUntil((r) {
      name ??= r.settings.name;
      return true;
    });
    if (name != null && name!.isNotEmpty) return name;
  } catch (_) {}
  return null;
}

/// A stable developer key string for an element's widget, or null. ONLY
/// LocalKeys with a deterministic value are accepted: `ValueKey<T>` and the
/// `Key('x')` factory (which is a `ValueKey<String>`). UniqueKey and GlobalKey
/// are rejected because they are allocated fresh per build (non-deterministic,
/// so useless as a stable anchor). The returned string round-trips through
/// `ValueKey<String|int>(...)` for find.byKey-based replay.
String? keyStringOf(Widget w) {
  final k = w.key;
  if (k is ValueKey<String>) return 's:${k.value}';
  if (k is ValueKey<int>) return 'i:${k.value}';
  if (k is ValueKey) return 'v:${k.value}';
  return null;
}

/// The raw developer-id VALUE from a keyString (strips the `s:`/`i:`/`v:` type
/// prefix). This is what enters the canonical descriptor as `@<id>`, matching
/// how the oracle/SDK treat a Key's value as the stable id. The prefixed
/// keyString is still used for `key:<keyString>` SELECTORS (replay).
String keyValueOf(String ks) {
  if (ks.startsWith('s:') || ks.startsWith('i:') || ks.startsWith('v:')) {
    return ks.substring(2);
  }
  return ks;
}

/// Rebuild a Finder-usable Key from a keyString produced by keyStringOf, for
/// the typed cases we can reconstruct exactly. String/int round-trip; anything
/// else falls back to a string ValueKey on the rendered value (best effort).
Key keyFromString(String ks) {
  if (ks.startsWith('s:')) return ValueKey<String>(ks.substring(2));
  if (ks.startsWith('i:')) {
    return ValueKey<int>(int.tryParse(ks.substring(2)) ?? 0);
  }
  return ValueKey<String>(ks.startsWith('v:') ? ks.substring(2) : ks);
}

/// True when [w] is the root of a subtree that is NOT on the current visible
/// screen, so its keyed elements must be pruned from the collection walk.
///
/// Why this matters: when a screen is reached via Navigator.push, the route(s)
/// underneath stay MOUNTED in the element tree but are taken OFFSTAGE by the
/// framework (a `ModalRoute` whose `offstage` is true is wrapped in an
/// `Offstage(offstage: true)`, and inactive route subtrees also have their
/// `TickerMode` disabled). The semantics walk in `snapshot()` already drops
/// these (their nodes carry `SemanticsFlag.isHidden`), so the visible tappables
/// list only holds onstage nodes. The key collection therefore has to match:
/// if it kept walking offstage routes it would return their keys in document
/// order and the index-based pairing would bind the visible (pushed-route)
/// tappables to the wrong, offstage keys. Pruning here keeps the two lists in
/// lock-step so keyed elements on a pushed route are addressable.
///
/// Detection uses only public, locale-invariant widget signals:
///   * `Offstage(offstage: true)` - inactive ModalRoute / explicitly offstage,
///   * `TickerMode(enabled: false)` - inactive route subtree (animations off),
///   * `Visibility(visible: false)` that does not maintain interactivity.
bool _isOffstageSubtree(Widget w) {
  if (w is Offstage) return w.offstage;
  if (w is TickerMode) return !w.enabled;
  if (w is Visibility) return !w.visible && !w.maintainInteractivity;
  return false;
}

/// Collect every stable developer key present in the live element tree, in
/// document order, as keyString values. Walking the ELEMENT tree (not the
/// semantics tree) is required: developer keys live on Widgets, not on
/// SemanticsData. Order-stable and locale-invariant. Offstage subtrees (routes
/// pushed under the current one) are pruned so the result reflects only the
/// CURRENT visible screen, matching the onstage semantics walk in snapshot().
List<String> collectKeys() {
  final keys = <String>[];
  void walk(Element e) {
    if (_isOffstageSubtree(e.widget)) return;
    final ks = keyStringOf(e.widget);
    if (ks != null) keys.add(ks);
    e.visitChildren(walk);
  }

  final root = WidgetsBinding.instance.rootElement;
  if (root != null) root.visitChildren(walk);
  return keys;
}

/// Crude locale-invariant role of an element, by widget runtime type, used ONLY
/// to pair a keyed element with a tappable semantics node of the same role.
/// Type names are stable and language-independent. Returns null for elements
/// that aren't a recognizable interactive control.
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
    // Generic tappables map to the canonical `button` role (matches roleOf).
    return 'button';
  }
  if (t.contains('Image')) return 'image';
  return null;
}

/// Keyed interactive elements ON THE CURRENT SCREEN, in document order:
/// (keyString, role). Lets a tappable semantics node be addressed by its
/// developer key when one exists. Offstage subtrees (e.g. the Home/List routes
/// that stay mounted underneath a pushed Detail route) are pruned via
/// [_isOffstageSubtree], so this list lines up index-for-index with the onstage
/// tappables collected from the semantics tree in snapshot(). Without the prune,
/// the index pairing would bind a pushed route's visible tappables to the wrong,
/// offstage keys and the real keys (e.g. detail_danger) would never be emitted.
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

/// Clip a label to the cap WITHOUT dropping its element. A label <= cap is
/// returned unchanged (signatures stay byte-identical for short labels). A
/// longer label is truncated to (cap - 9) code units + '#' + an 8-hex FNV-1a
/// hash of the FULL label, so long-named widgets stay in the snapshot and stay
/// tappable, distinct long labels keep distinct keys, and the result is
/// deterministic. findTappable() clips candidates the same way to resolve them.
String clipLabel(String label) {
  if (label.length <= maxLabelLen) return label;
  final suffix = '#${fnv1a(label)}';
  return label.substring(0, maxLabelLen - suffix.length) + suffix;
}

void visit(SemanticsNode node, void Function(SemanticsData) f) {
  final data = node.getSemanticsData();
  f(data);
  node.visitChildren((child) {
    visit(child, f);
    return true;
  });
}

SemanticsNode? _semanticsRoot(WidgetTester tester) {
  // WidgetTester owns a child pipeline. rootPipelineOwner points at a distinct
  // tree with no app semantics, so the deprecated compatibility getter remains
  // necessary until Flutter exposes the active test pipeline another way.
  // ignore: deprecated_member_use
  return tester.binding.pipelineOwner.semanticsOwner?.rootSemanticsNode;
}

/// A tappable element addressed STRUCTURALLY, never by localized text.
///   sel    canonical, locale-invariant selector for replay:
///            `key:<keyString>`   when the element has a stable developer key
///            `role:<role>#<idx>` otherwise (role + per-role structural index)
///   role   the locale-invariant role token (button, link, tappable, ...)
///   index  per-role structural index (document order among same-role taps)
///   key    the keyString if present, else null
///   label  the visible (localized) text, DISPLAY-ONLY: shown in map --show,
///          never folded into the signature or into `sel`.
class Tappable {
  Tappable(
    this.sel,
    this.role,
    this.index,
    this.key,
    this.label,
    this.inputPurpose,
  );
  final String sel;
  final String role;
  final int index;
  final String? key;
  final String label;
  final String? inputPurpose;
  bool get hasKey => key != null;
}

class Snapshot {
  Snapshot(
    this.tree,
    this.anchor,
    this.sig,
    this.labels,
    this.tappables,
    this.contentFp,
  );

  /// The captured canonical node tree (screen-rooted). Kept so the explorer can
  /// re-sign it with the Layer 2 per-node value-class CAP applied (capped keys
  /// dropped from the `V:` section). The raw `sig` below is the UNCAPPED canonical
  /// signature; `effectiveSig` re-signs with capped keys excluded.
  final RNode tree;

  /// The screen anchor (route template) that prefixes the signature.
  final String? anchor;

  /// STRUCTURAL + value-state CANONICAL signature: FNV-1a over the canonical
  /// descriptor (anchor prefix + normalized role/type/icon/id tree + the Layer 2
  /// `V:` value-class section). NO localized text contributes to the body. Same
  /// screen in English and German hashes identically; it matches the Rust oracle
  /// and the production SDK.
  final String sig;

  /// DISPLAY-ONLY visible text labels, for map --show human readability. Never
  /// part of the signature.
  final List<String> labels;

  /// Tappable elements, addressed structurally (key, else role+index).
  final List<Tappable> tappables;

  /// Layer 1 content fingerprint (runner-local, docs/signature.md): the
  /// structural+value signature PLUS sorted (stable-key, trimmed raw text) over
  /// text-bearing nodes. NEVER enters the canonical graph key (it carries raw
  /// localized text). Used only to decide if an action was EFFECTIVE: if the sig
  /// OR this fingerprint changed, something happened; if neither moved, the
  /// action was a no-op. This is what stops the explorer stalling on value-state
  /// screens whose structure never changes.
  final String contentFp;

  /// The canonical signature re-computed with the per-node value-class CAP
  /// applied: any value-key in [cappedKeys] is dropped from the `V:` section so
  /// an adversarial value generator (>8 distinct value-classes for one node)
  /// cannot explode the graph. With no capped keys this equals [sig].
  String effectiveSig(Set<String> cappedKeys) =>
      cappedKeys.isEmpty ? sig : signatureFrom(anchor, tree, cappedKeys);
}

Snapshot snapshot(WidgetTester t) => snapshotWith(t, const <String>{});

/// Build a [Snapshot]. [valueSelectors] is the Layer 3 `value_nodes:` opt-in set
/// (`key:<id>` / `role:<role>#<idx>`): a node matching one is marked
/// value-bearing even when its role is not a value-role.
Snapshot snapshotWith(WidgetTester t, Set<String> valueSelectors) {
  final labels = <String>[];
  final rawTaps = <_TapNode>[]; // tappable nodes in document order
  // (stable-key, trimmed raw text) over text-bearing nodes -> Layer 1 content fp.
  final textParts = <String>[];
  // Developer ids matched to canonical-role nodes in document order. Walking the
  // ELEMENT tree is required because keys live on Widgets, not SemanticsData.
  final keyedIdsByRole = <String, List<String>>{};
  for (final kt in collectKeyedTappables()) {
    (keyedIdsByRole[kt.value] ??= <String>[]).add(keyValueOf(kt.key));
  }
  final perRoleId = <String, int>{};
  // Global per-normalized-role document-order index, for resolving a Layer 3
  // `role:<role>#<idx>` value selector against this screen.
  final perRoleSel = <String, int>{};

  // Build the CANONICAL node tree (roles + types + ids + values), wrapped in a
  // `screen` root. The same walk captures DISPLAY-ONLY labels, the tappables
  // list, and the Layer 1 content fingerprint parts.
  final root = _semanticsRoot(t);
  final rootChildren = <RNode>[];
  if (root != null) {
    RNode? build(SemanticsNode node) {
      final data = node.getSemanticsData();
      if (data.flagsCollection.isHidden) {
        final kids = <RNode>[];
        node.visitChildren((c) {
          final b = build(c);
          if (b != null) kids.add(b);
          return true;
        });
        if (kids.isEmpty) return null;
        return RNode(role: 'group', children: kids);
      }
      final role = roleOf(data);
      final type = inputTypeOf(data, role);
      // Match a developer id by canonical role in document order.
      final idx = perRoleId[role] ?? 0;
      perRoleId[role] = idx + 1;
      final roleIds = keyedIdsByRole[role];
      final id = (roleIds != null && idx < roleIds.length)
          ? roleIds[idx]
          : null;

      // Layer 2 value-state: a value-role node's displayed value (text field,
      // slider, live region). Layer 3 opt-in: a node matching a `value_nodes:`
      // selector (by id, else by role+structural-index) is forced value-bearing.
      final selIdx = perRoleSel[role] ?? 0;
      perRoleSel[role] = selIdx + 1;
      final matchesSelector =
          (id != null && valueSelectors.contains('key:$id')) ||
          valueSelectors.contains('role:$role#$selIdx');
      var value = valueOf(data);
      var valueNode = value != null && valueNodeFlagOf(data);
      if (matchesSelector) {
        // Force value-bearing via the Layer 3 flag; source a value if the node
        // does not already expose one (best-effort: value, else label).
        value ??= data.value.trim().isNotEmpty ? data.value : data.label;
        valueNode = true;
      }

      // Multiline labels (e.g. "Compose\nTab 2 of 3") normalize to first line.
      final label = data.label.trim().split('\n').first.trim();
      final tappable =
          data.hasAction(SemanticsAction.tap) &&
          !data.flagsCollection.isTextField;
      if (label.isNotEmpty) labels.add(clipLabel(label));
      if (tappable || data.flagsCollection.isTextField) {
        rawTaps.add(_TapNode(role, clipLabel(label), inputTypeOf(data, role)));
      }

      // Layer 1: text-bearing parts (stable-key + trimmed raw text). The raw
      // value of a value node and the raw label of a text node both count, so a
      // counter whose display value changes registers as content movement even
      // when structure and value-CLASS are unchanged (e.g. 41 -> 42 stays POS2).
      final stableKey = id != null
          ? 'key:$id'
          : 'role:${normalizeRole(role)}#$selIdx';
      final rawText = (value ?? '').trim();
      final rawLabel = label;
      if (rawText.isNotEmpty) textParts.add('$stableKey$rawText');
      if (rawLabel.isNotEmpty) textParts.add('$stableKey$rawLabel');

      final kids = <RNode>[];
      node.visitChildren((c) {
        final b = build(c);
        if (b != null) kids.add(b);
        return true;
      });
      return RNode(
        role: role,
        id: id,
        type: type,
        value: value,
        valueNode: valueNode,
        children: kids,
      );
    }

    root.visitChildren((c) {
      final b = build(c);
      if (b != null) rootChildren.add(b);
      return true;
    });
  }

  // CANONICAL signature: descriptor of the screen-rooted tree, prefixed by the
  // screen anchor (route template). Matches crates/reproit/src/model/signature.rs.
  final anchor = screenAnchor(t);
  final tree = RNode(role: 'screen', children: rootChildren);
  final sig = signature(anchor, tree);

  // Layer 1 content fingerprint: structural+value sig + sorted text parts. Raw
  // text is included here ONLY; it never enters `sig` / the canonical graph key.
  final sortedText = textParts.toList()..sort();
  final contentFp = fnv1a('$sig ${sortedText.join(' ')}');

  // Build structural selectors. Each tappable maps to a developer KEY when one
  // exists (preferred: replays in any locale), else falls back to role +
  // per-role structural index. Keyed interactive elements are harvested in
  // document order and paired to semantics tappables of the same role in
  // document order. A tappable with no key keeps role+index and is flagged so
  // the map layer can later warn the developer to add a key.
  final keyedByRole = <String, List<String>>{};
  for (final kt in collectKeyedTappables()) {
    (keyedByRole[kt.value] ??= <String>[]).add(kt.key);
  }
  final tappables = <Tappable>[];
  final perRole = <String, int>{};
  for (final tn in rawTaps) {
    final idx = perRole[tn.role] ?? 0;
    perRole[tn.role] = idx + 1;
    final roleKeys = keyedByRole[tn.role];
    final key = (roleKeys != null && idx < roleKeys.length)
        ? roleKeys[idx]
        : null;
    final sel = key != null ? 'key:$key' : 'role:${tn.role}#$idx';
    String? purpose;
    final marker = key?.split('reproit-purpose-');
    if (marker != null && marker.length > 1) {
      purpose = marker[1].split('--').first;
    }
    if (purpose == null && tn.type == 'password') purpose = 'password';
    tappables.add(Tappable(sel, tn.role, idx, key, tn.label, purpose));
  }

  final unique = labels.toSet().toList();
  return Snapshot(tree, anchor, sig, unique, tappables, contentFp);
}

/// Internal: a tappable semantics node captured during the structural walk.
class _TapNode {
  _TapNode(this.role, this.label, this.type);
  final String role;
  final String label;
  final String? type;
}

/// Frame-timing capture: real per-frame UI (build) and raster durations,
