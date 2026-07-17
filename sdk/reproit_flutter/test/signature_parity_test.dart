import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:reproit_flutter/reproit_flutter.dart';

// THE Flutter parity gate. It loads the canonical golden vectors at the repo
// root (`signature_vectors.json`) and asserts the Dart implementation produces
// `expected_sig` for every vector, exactly as the Rust oracle's
// `tests::golden_vectors_match` does. If a vector mismatches, the failure prints
// the descriptor string so you can diff it against docs/signature.md before
// touching anything. Never edit the vectors or the oracle to make this pass.
//
// The descriptor that gets hashed is byte-identical to the Rust oracle:
//   token = <depth>:<role>[:<type>][#<icon>][@<id>] (trailing `*` on a repeat)
//   desc  = "A:" + anchor + "\n" + tokens.join(";")
//   sig   = FNV-1a 32-bit over UTF-8(desc), 8-char lowercase hex.

/// Locate signature_vectors.json relative to this test file. The test runs with
/// CWD = sdk/reproit_flutter, so the repo root is two levels up.
File _vectorsFile() {
  final candidates = <String>[
    'signature_vectors.json',
    '../../signature_vectors.json',
    '../../../signature_vectors.json',
  ];
  for (final c in candidates) {
    final f = File(c);
    if (f.existsSync()) return f;
  }
  fail(
      'could not locate signature_vectors.json (cwd=${Directory.current.path})');
}

class _Vector {
  _Vector(this.description, this.anchor, this.tree, this.expectedSig);
  final String description;
  final String? anchor;
  final RNode tree;
  final String expectedSig;
}

List<_Vector> _loadVectors() {
  final raw = _vectorsFile().readAsStringSync();
  final list = (jsonDecode(raw) as List).cast<Map<String, dynamic>>();
  return list
      .map((j) => _Vector(
            j['description'] as String,
            j['anchor'] as String?,
            RNode.fromJson((j['tree'] as Map).cast<String, dynamic>()),
            j['expected_sig'] as String,
          ))
      .toList();
}

void main() {
  test('golden vectors match the canonical oracle (all current vectors)', () {
    final vectors = _loadVectors();
    // 15 structural/anchor vectors + 10 value-state/unicode vectors. The whole set must
    // pass byte-for-byte, including every Layer 2 value-state vector.
    expect(vectors.length, greaterThanOrEqualTo(25),
        reason: 'need >= 25 vectors, got ${vectors.length}');
    for (final v in vectors) {
      final got = ReproIt.signatureOfTree(v.anchor, v.tree);
      expect(
        got,
        v.expectedSig,
        reason: "vector '${v.description}' mismatch.\n"
            '  descriptor = ${descriptor(v.anchor, v.tree)}\n'
            '  expected ${v.expectedSig} got $got',
      );
    }
  });

  test('cross-vector relationships hold (mirrors the Rust oracle)', () {
    final vectors = _loadVectors();
    String by(String needle) => vectors
        .firstWhere((v) => v.description.contains(needle),
            orElse: () => fail('no vector matching "$needle"'))
        .expectedSig;

    final login = by('basic login');
    // text-exclusion + transient-drop all collapse to the basic login.
    expect(login, by('locale-invariance'));
    expect(login, by('transient-drop (spinner)'));
    expect(login, by('transient-drop (snackbar'));
    // collapse drops the count.
    expect(by('repeated-collapse (3 items)'), by('repeated-collapse (5 items'));
    // discriminators split.
    expect(login, isNot(by('collision-fix via input type')));
    expect(login, isNot(by('collision-fix via icon')));
    expect(by('collision-fix via input type'),
        isNot(by('collision-fix via icon')));
    // anchor semantics.
    final settings = by('same route + same structure');
    expect(settings, isNot(by('different route + same structure')));
    expect(settings, isNot(by('same route + different structure')));
    expect(by('parameterized route (item 42)'),
        by('parameterized route (item 99)'));

    // value-state (Layer 2): EMPTY / ZERO / POS1 are three distinct states.
    final vEmpty = by('empty value-class');
    final vZero = by('zero value-class');
    final vPos1 = by('POS1 value-class');
    expect(vEmpty, isNot(vZero));
    expect(vEmpty, isNot(vPos1));
    expect(vZero, isNot(vPos1));
    // numeric counter 0 vs 5 -> ZERO vs POS1 distinct.
    expect(by('counter at 0'), isNot(by('counter at 5')));
    // A chrome label carrying a value is NOT value-bearing: it stays identical
    // to the same structure with no value field at all.
    {
      final structural = RNode(role: 'screen', children: [
        RNode(role: 'header', id: 'title'),
      ]);
      expect(ReproIt.signatureOfTree('/home', structural),
          by('chrome label with text'));
    }
    // grouped/locale number is locale-safe (NONEMPTY), distinct from numerics.
    final vGrouped = by('grouped/locale number');
    expect(vGrouped, isNot(vPos1));
    expect(vGrouped, isNot(vZero));
    // two different POS1 values (3 vs 7) bucket the same.
    expect(by('two different POS1 values bucket the same (3)'),
        by('two different POS1 values bucket the same (7)'));
  });

  test('value_class buckets + locale-safe fallback (mirrors the oracle)', () {
    expect(valueClass(''), 'EMPTY');
    expect(valueClass('   '), 'EMPTY');
    expect(valueClass('0'), 'ZERO');
    expect(valueClass('0.0'), 'ZERO');
    expect(valueClass('-0'), 'ZERO');
    expect(valueClass('-3'), 'NEG');
    expect(valueClass('-0.5'), 'NEG');
    expect(valueClass('3'), 'POS1');
    expect(valueClass('9.99'), 'POS1');
    expect(valueClass('+7'), 'POS1');
    expect(valueClass('10'), 'POS2');
    expect(valueClass('99'), 'POS2');
    expect(valueClass('100'), 'POS3');
    expect(valueClass('999.99'), 'POS3');
    expect(valueClass('1000'), 'POSL');
    expect(valueClass('123456'), 'POSL');
    expect(valueClass('  42  '), 'POS2');
    // locale-safe: ambiguous / grouped / non-ASCII -> NONEMPTY (no guessing).
    expect(valueClass('1,234'), 'NONEMPTY');
    expect(valueClass('1.234.567'), 'NONEMPTY');
    expect(valueClass('1 234'), 'NONEMPTY');
    expect(valueClass(r'$5'), 'NONEMPTY');
    expect(valueClass('5%'), 'NONEMPTY');
    expect(valueClass('1e3'), 'NONEMPTY');
    expect(valueClass('0x10'), 'NONEMPTY');
    expect(valueClass('.'), 'NONEMPTY');
    expect(valueClass('3.'), 'NONEMPTY');
    expect(valueClass('.5'), 'NONEMPTY');
    expect(valueClass('--5'), 'NONEMPTY');
    expect(valueClass('hello'), 'NONEMPTY');
    expect(valueClass('١٢٣'), 'NONEMPTY'); // Arabic-Indic digits
  });

  test('V: section is conditional and well-formed', () {
    // No value -> purely structural descriptor (no V: line).
    expect(descriptor(null, RNode(role: 'textfield', id: 'email')),
        'A:\n0:textfield@email');
    // A chrome node with a value is still not value-bearing: no V: line.
    expect(
        descriptor(null, RNode(role: 'header', id: 'title', value: 'Welcome')),
        'A:\n0:header@title');
    // A value-role node with a value appends a sorted V: section.
    expect(
        descriptor(
            null, RNode(role: 'textfield', id: 'email', value: 'a@b.com')),
        'A:\n0:textfield@email\nV:key:email=NONEMPTY');
    // status normalizes to node in the body but is value-bearing in V:.
    expect(descriptor(null, RNode(role: 'status', id: 'count', value: '5')),
        'A:\n0:node@count\nV:key:count=POS1');
    // Layer 3 opt-in: a chrome text role flagged value_node enters V:.
    expect(
        descriptor(null,
            RNode(role: 'text', id: 'display', value: '42', valueNode: true)),
        'A:\n0:text@display\nV:key:display=POS2');
    // Two keyless value nodes collapse in the body but keep distinct V: keys.
    final screen = RNode(role: 'screen', children: [
      RNode(role: 'textfield', value: '3'),
      RNode(role: 'textfield', value: '99'),
    ]);
    expect(descriptor(null, screen),
        'A:\n0:screen;1:textfield*\nV:role:textfield#0=POS1;role:textfield#1=POS2');
  });

  test('runner cap excludes a capped value-key from the V: section', () {
    final tf = RNode(role: 'textfield', id: 'amount', value: '5');
    expect(descriptorFrom(null, tf, <String>{}),
        'A:\n0:textfield@amount\nV:key:amount=POS1');
    // With the key capped it falls back to structural-only (no V: line), so an
    // adversarial value generator cannot explode the graph. The capped signature
    // equals the value-less structural signature of the same node.
    expect(descriptorFrom(null, tf, <String>{'key:amount'}),
        'A:\n0:textfield@amount');
    expect(signatureFrom(null, tf, <String>{'key:amount'}),
        signature(null, RNode(role: 'textfield', id: 'amount')));
  });

  test('FNV-1a known values', () {
    // "" -> the FNV-1a 32-bit offset basis itself.
    expect(fnv1a32(''), '811c9dc5');
    // Cross-check a known FNV-1a 32 value for "a" = 0xe40c292c.
    expect(fnv1a32('a'), 'e40c292c');
  });

  test('descriptor shape matches the spec (spot checks)', () {
    // Empty anchor still has the A: prefix line.
    expect(descriptor(null, RNode(role: 'screen')), 'A:\n0:screen');
    // Unknown role normalizes to node.
    expect(descriptor(null, RNode(role: 'carousel')), 'A:\n0:node');
    // Token field order: type, icon, id, then the repeat marker.
    expect(
      descriptor(null,
          RNode(role: 'textfield', type: 'password', icon: 'lock', id: 'pwd')),
      'A:\n0:textfield:password#lock@pwd',
    );
    // Repeated siblings collapse to one *-marked token, count dropped.
    final list = RNode(role: 'list', children: [
      RNode(role: 'listitem', children: [RNode(role: 'text')]),
      RNode(role: 'listitem', children: [RNode(role: 'text')]),
      RNode(role: 'listitem', children: [RNode(role: 'text')]),
    ]);
    expect(descriptor(null, list), 'A:\n0:list;1:listitem*;2:text');
  });
}
