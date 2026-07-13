import 'package:flutter/material.dart';

void main() => runApp(const FixtureApp());

class FixtureApp extends StatelessWidget {
  const FixtureApp({super.key});

  @override
  Widget build(BuildContext context) => const MaterialApp(home: FixtureScreen());
}

class FixtureScreen extends StatefulWidget {
  const FixtureScreen({super.key});

  @override
  State<FixtureScreen> createState() => _FixtureScreenState();
}

class _FixtureScreenState extends State<FixtureScreen> {
  bool revealed = false;

  @override
  Widget build(BuildContext context) => Scaffold(
        body: Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              ElevatedButton(
                key: const ValueKey('toggle'),
                onPressed: () => setState(() => revealed = true),
                child: const Text('Toggle'),
              ),
              if (revealed)
                const Text('Detail revealed', key: ValueKey('detail')),
            ],
          ),
        ),
      );
}
