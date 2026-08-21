import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:xterm/xterm.dart';

class RustDeskTerminal extends Terminal {
  RustDeskTerminal({super.maxLines}) {
    onPrivateOSC = _handlePrivateOsc;
  }

  static const _clipboardOscCode = '52';
  static const _systemClipboardSelection = 'c';
  static final _osc52Selection = RegExp(r'^[cpqs0-7]*$');

  void _handlePrivateOsc(String code, List<String> args) {
    if (code != _clipboardOscCode) return;
    if (args.length != 2 || !_osc52Selection.hasMatch(args.first)) {
      debugPrint('[RustDeskTerminal] Rejected malformed OSC 52 command');
      return;
    }
    if (args.last == '?') {
      debugPrint('[RustDeskTerminal] Rejected OSC 52 clipboard query');
      return;
    }
    final selection = args.first;
    if (selection.isNotEmpty &&
        !selection.contains(_systemClipboardSelection)) {
      debugPrint('[RustDeskTerminal] Ignored unsupported OSC 52 selection');
      return;
    }
    if (selection.replaceAll(_systemClipboardSelection, '').isNotEmpty) {
      debugPrint('[RustDeskTerminal] Ignored unsupported OSC 52 selections');
    }
    try {
      final text = utf8.decode(base64.decode(args.last));
      unawaited(Clipboard.setData(ClipboardData(text: text)));
    } on FormatException {
      debugPrint('[RustDeskTerminal] Rejected malformed OSC 52 payload');
    }
  }

  @override
  void eraseScrollbackOnly() {
    final scrollBack = buffer.scrollBack;
    if (scrollBack == 0) return;

    // Selection anchors require retained buffer lines to be reindexed.
    buffer.lines.remove(0, scrollBack);
  }
}
