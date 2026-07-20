// Reproit simulator explorer entry point.

import 'package:integration_test/integration_test.dart';

import 'reproit_explorer.dart';

// APP-SPECIFIC: import your app's root widget.
// import 'package:your_app/app.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  registerExplorer(
    runtime: const SimulatorExplorerRuntime(),
    pumpApp: (t) async {
      // await t.pumpWidget(const YourApp());
    },
  );
}
