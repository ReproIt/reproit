import 'package:flutter_test/flutter_test.dart';
import 'package:reproit_flutter/reproit_flutter.dart';

// Verifies tier-1 auto dimensions + the identify/setContext API + hashed uid.
void main() {
  tearDown(ReproIt.dispose);

  testWidgets('captures auto dimensions and a hashed uid', (tester) async {
    ReproIt.init(const ReproItConfig(appId: 'ctx', onEvent: _noop));

    final ctx = ReproIt.context;
    // tier-1 auto dimensions (zero-PII, web-safe)
    expect(ctx.containsKey('platform'), isTrue);
    expect(ctx.containsKey('locale'), isTrue);
    expect(ctx.containsKey('tz'), isTrue);
    expect(ctx['release'], isA<bool>());

    // identify hashes the raw id (never stored in the clear) and is stable
    ReproIt.identify('user@example.com', context: {'role': 'admin'});
    final uid = ReproIt.context['uid'] as String;
    expect(uid, matches(RegExp(r'^[0-9a-f]{16}$')));
    expect(uid, isNot(contains('user'))); // not the raw value
    expect(ReproIt.context['role'], 'admin');

    // setContext merges further dimensions
    ReproIt.setContext('plan', 'free');
    expect(ReproIt.context['plan'], 'free');

    ReproIt.dispose();
  });
}

void _noop(Map<String, dynamic> _) {}
