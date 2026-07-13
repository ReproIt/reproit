// Operability validation fixture for the ReproIt Flutter groundtruth emitter.
//
// Two side-by-side tappables that mirror the WPF/AppKit "fake button" cases:
//
//   1. realButton  - a genuine ElevatedButton. It is operable (onPressed),
//      carries a real `button` role in the semantics tree, a label, and owns a
//      Focus node so it is in the tab order and keyboard-activatable. NO gap.
//
//   2. fakeButton  - a bare GestureDetector(onTap: ...) wrapping a Text. It is
//      operable by pointer (live onTap, hit-testable), but it has NO Button
//      semantics/role and NO Focus, so it is NOT in the tab order and NOT
//      keyboard-activatable. This is a real operability GAP: the engine must
//      flag rolePresent=false AND keyboardActivatable=false.
//
// The two are kept structurally distinct (keyed) so the emitted EXPLORE:
// GROUNDTRUTH entries are unambiguous.

import 'package:flutter/material.dart';

class OperabilityFixtureApp extends StatelessWidget {
  const OperabilityFixtureApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      debugShowCheckedModeBanner: false,
      theme: ThemeData(useMaterial3: false),
      home: Scaffold(
        body: Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              // CLEAN: a real, accessible button.
              ElevatedButton(
                key: const ValueKey<String>('real_button'),
                onPressed: () {},
                child: const Text('Buy'),
              ),
              const SizedBox(height: 24),
              // GAP: operable by pointer, but no button role + not keyboard-
              // activatable. A "fake button".
              GestureDetector(
                key: const ValueKey<String>('fake_button'),
                onTap: () {},
                child: Container(
                  padding: const EdgeInsets.all(16),
                  color: const Color(0xFF2196F3),
                  child: const Text('Checkout'),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
