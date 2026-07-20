// Reproit headless explorer entry point.

import 'package:flutter_test/flutter_test.dart';

import '../integration_test/reproit_explorer.dart';

// APP-SPECIFIC: import your app's root widget.
// import 'package:your_app/app.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  registerExplorer(
    runtime: const HeadlessExplorerRuntime(),
    pumpApp: (t) async {
      // await t.pumpWidget(const YourApp());
    },
  );
}
