import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:reproit_flutter/reproit_flutter.dart';

// Privacy contract (docs/data-handling.md): "Password and hidden fields ... are
// never read at all, not even to fingerprint them." This proves the Flutter SDK
// honors it: an obscured (obscureText / password) text field is NOT included in
// the collected field fingerprints, while a normal text field IS. The real
// password length and the field's identity therefore never leave the device.
void main() {
  tearDown(ReproIt.dispose); // idempotent safety net

  testWidgets(
      'obscured (password) fields are skipped; normal text fields are fingerprinted',
      (tester) async {
    ReproIt.init(ReproItConfig(
      appId: 'test',
      // Swallow events; this test inspects collectFieldFingerprints directly.
      onEvent: (_) {},
      debounce: const Duration(milliseconds: 50),
    ));

    await tester.pumpWidget(const _FormApp());
    await tester.pump(const Duration(milliseconds: 80)); // settle semantics

    // Type into both fields so each has a non-trivial value. The password is a
    // distinctive length so that, if it WERE leaked, `len` would reveal it.
    await tester.enterText(find.byKey(const ValueKey('email')), 'a@b.co');
    await tester.enterText(
        find.byKey(const ValueKey('password')), 'hunter2-very-secret-123');
    await tester.pump(const Duration(milliseconds: 80));

    final fps = ReproIt.collectFieldFingerprints();

    // Dispose now so a failing assertion can't leak timers/semantics handle.
    ReproIt.dispose();

    // The normal (email) field IS fingerprinted.
    final emailFp = fps.where((f) => f['field'] == 'Email').toList();
    expect(emailFp, hasLength(1),
        reason: 'a non-obscured text field must be fingerprinted');
    expect(emailFp.single['len'], 6); // 'a@b.co'
    expect(emailFp.single['isEmpty'], false);

    // The obscured (password) field is NOT fingerprinted at all: no entry whose
    // field name OR whose leaked length could correspond to the password.
    final passwordFp = fps.where((f) => f['field'] == 'Password').toList();
    expect(passwordFp, isEmpty,
        reason: 'an obscured (password) field must never be fingerprinted');

    // Defense in depth: the real password length (23) must not appear in ANY
    // collected fingerprint, regardless of how the field is named/labeled.
    for (final f in fps) {
      expect(f['len'], isNot(23),
          reason: 'password length must never leak into any fingerprint');
    }
  });
}

class _FormApp extends StatelessWidget {
  const _FormApp();
  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: Scaffold(
        body: Column(
          mainAxisSize: MainAxisSize.min,
          children: const [
            TextField(
              key: ValueKey('email'),
              decoration: InputDecoration(labelText: 'Email'),
            ),
            TextField(
              key: ValueKey('password'),
              obscureText: true, // -> SemanticsFlag.isObscured
              decoration: InputDecoration(labelText: 'Password'),
            ),
          ],
        ),
      ),
    );
  }
}
