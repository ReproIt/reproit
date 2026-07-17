part of '../reproit_explorer.dart';

// ===========================================================================
// OPERABILITY / ACCESSIBILITY GROUND-TRUTH (EXPLORE:GROUNDTRUTH).
//
// Two graphs, joined per element:
//   GRAPH 1 (operability): the live WIDGET/ELEMENT tree. An element is operable
//     iff it carries a LIVE interactive affordance (a non-null gesture callback /
//     non-empty recognizer / an actionable control TYPE) AND is hit-testable
//     (its RenderBox has a non-empty size and an on-screen centre).
//   GRAPH 2 (accessibility): the semantics tree (same tree EXPLORE:STATE signs).
//     Each operable element joins to the SMALLEST semantics rect containing its
//     hit-test centre; rolePresent = that node has a real role, namePresent = it
//     carries a label/tooltip/value.
//   KEYBOARD: FocusManager.instance.rootScope traversal order -> inTabOrder /
//     focusable; activation via the framework's default Actions is approximated
//     by "focusable AND in the tab order" (a bare GestureDetector has neither).
//
// Engine rule (reproit): an operable element is an a11y GAP iff
// keyboardActivatable==false OR inTabOrder==false OR rolePresent==false. We only
// emit dims we actually determined; missing dims default true (no gap) on the
// engine side. PUBLIC API only (widget.onTap, e.renderObject, RenderBox,
// FocusManager) so it survives profile/AOT; no WidgetInspector RPCs.
// ===========================================================================

/// An operable widget found in graph 1: a hit-testable element with a live
/// interactive affordance. `element` is the live Element (for focus-ancestry
/// attribution); `point` is its on-screen hit-test centre in SEMANTICS (physical)
/// space, used to join it to a semantics node.
class _Operable {
  _Operable(
    this.gestureKind,
    this.role,
    this.keyString,
    this.element,
    this.point,
  );
  final String gestureKind;
  final String role;
  final String? keyString;
  final Element element;
  final Offset point;
  FocusNode? focusNode; // attributed from the tab order by render-ancestry.
}

/// The on-screen hit-test centre of [e]'s RenderBox, or null when the element is
/// not laid out, not a box, or has zero area. Public API only (renderObject,
/// RenderBox.hasSize/size/localToGlobal).
Offset? _hitPoint(Element e) {
  final ro = e.renderObject;
  if (ro is! RenderBox) return null;
  if (!ro.hasSize) return null;
  final size = ro.size;
  if (size.isEmpty) return null;
  try {
    return ro.localToGlobal(size.center(Offset.zero));
  } catch (_) {
    return null;
  }
}

/// gestureKind ("tap"|"button"|"field"|"raw") for an operable widget, or null
/// when [w] has no LIVE affordance. Checks the runtime TYPE (locale-invariant)
/// AND the public callback fields, so a GestureDetector with onTap==null (and no
/// other live callback) is correctly NOT operable.
String? _operableKind(Widget w) {
  if (w is GestureDetector) {
    final live =
        w.onTap != null ||
        w.onDoubleTap != null ||
        w.onLongPress != null ||
        w.onTapDown != null ||
        w.onTapUp != null;
    return live ? 'tap' : null;
  }
  if (w is InkResponse) {
    // InkWell extends InkResponse.
    final live =
        w.onTap != null ||
        w.onDoubleTap != null ||
        w.onLongPress != null ||
        w.onTapDown != null;
    return live ? 'tap' : null;
  }
  if (w is RawGestureDetector) {
    return w.gestures.isNotEmpty ? 'raw' : null;
  }
  if (w is ListTile) {
    final live = w.onTap != null || w.onLongPress != null;
    return live ? 'button' : null;
  }
  final t = w.runtimeType.toString();
  if (t.contains('EditableText') ||
      t.contains('TextField') ||
      t.contains('TextFormField') ||
      t.contains('CupertinoTextField')) {
    return 'field';
  }
  if (t.contains('Switch') ||
      t.contains('Checkbox') ||
      t.contains('Radio') ||
      t.contains('Slider') ||
      t.contains('Button') ||
      t.contains('Chip') ||
      t.contains('Tab')) {
    return 'button';
  }
  return null;
}

/// Locale-invariant role token for an operable element, matching elementRole():
/// generic tappables (GestureDetector/InkWell/ListTile/raw) -> `button`.
String _operableRole(Widget w) {
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
  return 'button';
}

/// A semantics node reduced to (id, global rect, role, named) for the graph-2
/// join. `id` is SemanticsNode.id, used to collapse the several operable widgets
/// of one Material control (its outer keyed widget, its InkWell, its internal
/// RawGestureDetector) that all join to the SAME semantics node into one entry.
class _SemRect {
  _SemRect(this.id, this.rect, this.role, this.named);
  final int id;
  final Rect rect;
  final String role;
  final bool named;
}

/// Global rect of a semantics node, composing ancestor transforms (each
/// SemanticsNode.transform maps the node into its parent's coordinates).
Rect _globalRect(SemanticsNode node) {
  var matrix = Matrix4.identity();
  SemanticsNode? n = node;
  while (n != null) {
    final tr = n.transform;
    if (tr != null) matrix = tr.multiplied(matrix);
    n = n.parent;
  }
  return MatrixUtils.transformRect(matrix, node.rect);
}

/// The smallest-area semantics rect that contains [p], or null.
_SemRect? _smallestContaining(List<_SemRect> nodes, Offset p) {
  _SemRect? best;
  var bestArea = double.infinity;
  for (final s in nodes) {
    if (s.rect.contains(p)) {
      final area = s.rect.width * s.rect.height;
      if (area < bestArea) {
        bestArea = area;
        best = s;
      }
    }
  }
  return best;
}

/// Whether keyboard focus is confined to a sub-region it can't tab out of.
///
/// Reported CONSERVATIVELY as false. A real focus trap can only be told apart
/// from a legitimate modal by actually stepping the [FocusTraversalPolicy]
/// (next()/previous()) and observing focus never leave a region, which MUTATES
/// the live focus state. This snapshot must stay side-effect-free (it runs in
/// the middle of the seeded walk), so it does not drive traversal. Static scope
/// flags do NOT distinguish a trap from normal nesting: the framework marks the
/// root scope, every route scope, and each FocusTraversalGroup
/// `TraversalEdgeBehavior.closedLoop` BY DEFAULT, so a closedLoop scope is the
/// norm, not a trap signal. Emitting a guess here would feed the engine false
/// gaps. A dedicated key-driven trap oracle is the place to determine this.
bool _detectFocusTrap(FocusScopeNode rootScope) => false;

/// True when [w] roots a subtree that takes NO pointer input or is excluded
/// from semantics, so its gesture detectors are framework chrome, not real user
/// affordances. The chief offender is the route's `ModalBarrier`, whose
/// `_ModalBarrierGestureDetector` (a RawGestureDetector) sits under
/// `IgnorePointer` + `ExcludeSemantics` when no dialog is up; without this prune
/// it surfaces as a phantom operable `raw` element joined to no semantics node.
bool _isInertSubtree(Widget w) {
  if (w is IgnorePointer) return w.ignoring;
  if (w is AbsorbPointer) return w.absorbing;
  if (w is ExcludeSemantics) return w.excluding;
  return false;
}

/// Build the EXPLORE:GROUNDTRUTH payload for the current screen. [sig] MUST be
/// the SAME signature emitted on the paired EXPLORE:STATE so the engine joins
/// the two markers. Returns a JSON-ready map:
///   {"sig":..,"focusTrap":bool,"elements":[{id,operable,gestureKind,a11y{..}}]}
Map<String, dynamic> groundTruth(WidgetTester t, String sig) {
  // GRAPH 1: operable widgets in the live, on-screen element tree (offstage
  // subtrees pruned, exactly like the key/tappable walks).
  // Semantics rects are in PHYSICAL (device) pixels; RenderBox.localToGlobal
  // returns LOGICAL pixels. Scale operable hit points by the devicePixelRatio so
  // both graphs share one coordinate space for the geometric join.
  final dpr = t.view.devicePixelRatio;
  final operables = <_Operable>[];
  void walk(Element e) {
    if (_isOffstageSubtree(e.widget) || _isInertSubtree(e.widget)) return;
    final kind = _operableKind(e.widget);
    if (kind != null) {
      final pt = _hitPoint(e);
      if (pt != null) {
        operables.add(
          _Operable(
            kind,
            _operableRole(e.widget),
            keyStringOf(e.widget),
            e,
            pt * dpr,
          ),
        );
      }
    }
    e.visitChildren(walk);
  }

  final rootEl = WidgetsBinding.instance.rootElement;
  if (rootEl != null) rootEl.visitChildren(walk);

  // GRAPH 2: onstage semantics nodes as (id, global rect, role, named).
  final semNodes = <_SemRect>[];
  final root = _semanticsRoot(t);
  if (root != null) {
    void semWalk(SemanticsNode n) {
      final d = n.getSemanticsData();
      if (!d.flagsCollection.isHidden) {
        final named =
            d.label.trim().isNotEmpty ||
            d.tooltip.trim().isNotEmpty ||
            d.value.trim().isNotEmpty;
        semNodes.add(_SemRect(n.id, _globalRect(n), roleOf(d), named));
      }
      n.visitChildren((c) {
        semWalk(c);
        return true;
      });
    }

    semWalk(root);
  }

  // KEYBOARD: focus traversal order (tab order). Each FocusNode carries its
  // BuildContext (= the Focus element), so a node is ATTRIBUTED to the operable
  // element it lives inside, by render-ancestry. A control like ElevatedButton
  // owns its Focus node internally (the Focus widget's `focusNode` field is
  // null), so reading the widget field misses it; walking up from the node's
  // context to the enclosing operable element is what catches it.
  final fm = FocusManager.instance;
  final tabOrder = fm.rootScope.traversalDescendants.toList();
  final focusTrap = _detectFocusTrap(fm.rootScope);
  // Map each operable element to its nearest tab-order FocusNode by ancestry.
  final opIndexByElement = <Element, int>{};
  for (var i = 0; i < operables.length; i++) {
    opIndexByElement[operables[i].element] = i;
  }
  for (final fn in tabOrder) {
    final ctx = fn.context;
    if (ctx is! Element) continue;
    // Self-or-ancestor: the operable element enclosing this focus node.
    Element? hit;
    if (opIndexByElement.containsKey(ctx)) {
      hit = ctx;
    } else {
      ctx.visitAncestorElements((anc) {
        if (opIndexByElement.containsKey(anc)) {
          hit = anc;
          return false;
        }
        return true;
      });
    }
    if (hit != null) {
      final op = operables[opIndexByElement[hit]!];
      op.focusNode ??= fn; // first (nearest in tab order) wins.
    }
  }
  final tabOrderSet = tabOrder.toSet();

  // JOIN graph1 -> graph2 and COLLAPSE. One Material control expands into several
  // operable widgets (its outer keyed widget, its InkWell, its internal
  // RawGestureDetector) that all join to the SAME semantics node; they are one
  // logical control, so group operables by their joined semantics-node id and
  // emit ONE entry per group. The group is `operable` if any member is, has a
  // role/name if its shared semantics node does, and is focusable / in tab order
  // / keyboard-activatable if ANY member's attributed focus node says so. Within
  // a group the KEYED selector wins (else the first member's role+index), so the
  // entry's id matches the EXPLORE:STATE selector for the same control.
  // Operables that join to NO semantics node keep their own ungrouped entry
  // (these are the real gaps: operable but absent from the semantics graph).
  final groups = <int, List<int>>{}; // semantics node id -> operable indices
  final semForOp = <int, _SemRect>{};
  for (var i = 0; i < operables.length; i++) {
    final sem = _smallestContaining(semNodes, operables[i].point);
    if (sem != null) {
      semForOp[i] = sem;
      (groups[sem.id] ??= <int>[]).add(i);
    }
  }

  // Per-role structural index for keyless selectors, assigned in document order
  // over the COLLAPSED entries so it lines up with the EXPLORE:STATE indexing.
  final perRole = <String, int>{};
  String selectorFor(_Operable op) {
    if (op.keyString != null) return 'key:${op.keyString}';
    final idx = perRole[op.role] ?? 0;
    perRole[op.role] = idx + 1;
    return 'role:${op.role}#$idx';
  }

  final elements = <Map<String, dynamic>>[];
  void emitEntry(List<int> memberIdx, _SemRect? sem) {
    // Prefer the keyed member for the selector; else the first (document order).
    final lead = memberIdx.firstWhere(
      (i) => operables[i].keyString != null,
      orElse: () => memberIdx.first,
    );
    final op = operables[lead];
    final rolePresent = sem != null && sem.role != 'node';
    final namePresent = sem != null && sem.named;
    var focusable = false;
    var inTabOrder = false;
    for (final i in memberIdx) {
      final fn = operables[i].focusNode;
      if (fn == null) continue;
      if (fn.canRequestFocus && !fn.skipTraversal) focusable = true;
      if (tabOrderSet.contains(fn)) inTabOrder = true;
    }
    // keyboardActivatable: reachable by Tab (in the traversal order) AND
    // focusable, so the framework's default Enter/Space Actions can activate it.
    // A bare GestureDetector (no Focus) is neither, so this is false for it.
    final keyboardActivatable = inTabOrder && focusable;
    elements.add({
      'id': selectorFor(op),
      'operable': true,
      'gestureKind': op.gestureKind,
      'a11y': {
        'rolePresent': rolePresent,
        'namePresent': namePresent,
        'focusable': focusable,
        'inTabOrder': inTabOrder,
        'keyboardActivatable': keyboardActivatable,
      },
    });
  }

  // Emit in document order of the LEAD operable so output order is stable.
  final emittedGroups = <int>{};
  for (var i = 0; i < operables.length; i++) {
    final sem = semForOp[i];
    if (sem != null) {
      if (emittedGroups.add(sem.id)) emitEntry(groups[sem.id]!, sem);
    } else {
      emitEntry(<int>[i], null);
    }
  }

  return {'sig': sig, 'focusTrap': focusTrap, 'elements': elements};
}
