// Minimal Flutter Windows probe for todo 0057: proves posted window
// messages (src-tauri/src/supervisor/window/input.rs) drive mouse clicks
// and keyboard text into the FLUTTERVIEW child, which is where Flutter
// takes input, never the runner window itself.
//
// A big button increments a counter; a TextField (autofocus off, so a
// never-activated headless window starts with nothing focused, same as
// the Edge leg) reports every keystroke. Both write `<count>\n<text>` to
// the state file path passed as the first command-line argument, so a
// test driving this process headless can poll the file for proof the
// input landed.
import 'dart:io';

import 'package:flutter/material.dart';

late File _stateFile;
int _count = 0;
String _text = '';

void _writeState() {
  _stateFile.writeAsStringSync('$_count\n$_text');
}

void main(List<String> args) {
  final path = args.isNotEmpty ? args[0] : 'flutter_input_probe_state.txt';
  _stateFile = File(path);
  _writeState();
  runApp(const ProbeApp());
}

class ProbeApp extends StatelessWidget {
  const ProbeApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: Scaffold(
        body: Column(
          children: [
            SizedBox(
              width: double.infinity,
              height: 200,
              child: _CounterButton(),
            ),
            TextField(
              autofocus: false,
              style: const TextStyle(fontSize: 32),
              onChanged: (value) {
                _text = value;
                _writeState();
              },
            ),
          ],
        ),
      ),
    );
  }
}

class _CounterButton extends StatefulWidget {
  @override
  State<_CounterButton> createState() => _CounterButtonState();
}

class _CounterButtonState extends State<_CounterButton> {
  @override
  Widget build(BuildContext context) {
    return ElevatedButton(
      onPressed: () {
        setState(() => _count++);
        _writeState();
      },
      child: Text('clicks: $_count', style: const TextStyle(fontSize: 32)),
    );
  }
}
