import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:xterm/xterm.dart';

import 'terminal_clipboard_writer.dart'
    if (dart.library.html) 'terminal_clipboard_writer_web.dart';

const _controlShiftVPasteShortcut = SingleActivator(
  LogicalKeyboardKey.keyV,
  control: true,
  shift: true,
);

typedef TerminalClipboardWriter = Future<bool> Function(
  String text, {
  required bool userInitiated,
});

class TerminalClipboardNoticeRequest<T> {
  const TerminalClipboardNoticeRequest({
    required this.source,
    required this.text,
    required this.persistAllowed,
  });

  final T source;
  final String text;
  final bool persistAllowed;

  String get actionKey =>
      persistAllowed ? 'Enable clipboard' : 'Copy to clipboard';
}

const kTerminalClipboardNoticeMessageKey = 'terminal-clipboard-write-tip';

class TerminalClipboardNoticeCoordinator<T> {
  TerminalClipboardNoticeRequest<T>? _current;
  TerminalClipboardNoticeRequest<T>? _closing;
  bool _noticeVisible = false;
  bool _permissionWritePending = false;

  TerminalClipboardNoticeRequest<T>? get current => _current;

  TerminalClipboardNoticeRequest<T>? currentForSource(T source) {
    final current = _current;
    if (current == null || current.source != source) return null;
    return current;
  }

  Future<bool> recordBlocked({
    required T source,
    required String text,
    required String option,
    required bool canHandle,
    required bool Function(T source) canWrite,
    required Future<void> Function() persistDenial,
  }) async {
    if (!canHandle || !canWrite(source)) return false;
    final persistAllowed = option == kTerminalClipboardWriteUnconfigured;
    if (option != kTerminalClipboardWriteAllowed && !persistAllowed) {
      return false;
    }
    _current = TerminalClipboardNoticeRequest(
      source: source,
      text: text,
      persistAllowed: persistAllowed,
    );
    if (_permissionWritePending || _noticeVisible) return false;
    if (!persistAllowed) return true;

    _permissionWritePending = true;
    try {
      await persistDenial();
      final current = _current;
      return current != null && canWrite(current.source);
    } finally {
      _permissionWritePending = false;
    }
  }

  TerminalClipboardNoticeRequest<T>? startShowing(
    bool Function(T source) canWrite,
  ) {
    final current = _current;
    if (_noticeVisible || current == null) return null;
    if (!canWrite(current.source)) {
      _clearIfCurrent(current);
      return null;
    }
    _noticeVisible = true;
    return current;
  }

  bool beginClose({TerminalClipboardNoticeRequest<T>? expected}) {
    final current = _current;
    if (expected != null && !identical(current, expected)) return false;
    if (!_noticeVisible) {
      if (expected == null) {
        clear();
      } else {
        _clearIfCurrent(expected);
      }
      return false;
    }
    _closing = current;
    return true;
  }

  bool noticeClosed() {
    _noticeVisible = false;
    final closing = _closing;
    _closing = null;
    if (closing != null) _clearIfCurrent(closing);
    return _current != null;
  }

  void _clearIfCurrent(TerminalClipboardNoticeRequest<T> request) {
    if (identical(_current, request)) clear();
  }

  void clear() {
    _current = null;
    _closing = null;
    _noticeVisible = false;
  }
}

Future<bool> writeTerminalClipboard(
  String text, {
  bool userInitiated = false,
}) =>
    writeTerminalClipboardPlatform(text, userInitiated: userInitiated);

Future<bool> completeTerminalClipboardWrite({
  required String clipboardText,
  required bool Function() canWrite,
  required TerminalClipboardWriter writeClipboard,
  Future<void> Function()? persistAllowed,
}) async {
  if (!canWrite()) return false;
  if (!await writeClipboard(clipboardText, userInitiated: true)) return false;
  if (persistAllowed == null) return true;
  if (!canWrite()) return false;
  await persistAllowed();
  return true;
}

Map<ShortcutActivator, Intent>? platformTerminalShortcuts() {
  final platform = defaultTargetPlatform;
  if (platform == TargetPlatform.linux) {
    return {
      for (final entry in defaultTerminalShortcuts.entries)
        if (!_isControlShortcut(entry.key, LogicalKeyboardKey.keyV))
          entry.key: entry.value,
      _controlShiftVPasteShortcut:
          const PasteTextIntent(SelectionChangedCause.keyboard),
    };
  }
  if (platform != TargetPlatform.windows &&
      platform != TargetPlatform.android) {
    return null;
  }
  return {
    for (final entry in defaultTerminalShortcuts.entries)
      if (!_isControlShortcut(
        entry.key,
        LogicalKeyboardKey.keyC,
        shift: true,
      ))
        entry.key: entry.value,
  };
}

bool _isControlShortcut(
  ShortcutActivator shortcut,
  LogicalKeyboardKey key, {
  bool shift = false,
}) =>
    shortcut is SingleActivator &&
    shortcut.trigger == key &&
    shortcut.control &&
    shortcut.shift == shift &&
    !shortcut.alt &&
    !shortcut.meta;

FocusOnKeyEventCallback terminalCopyHandler(
  Terminal terminal,
  TerminalController controller, {
  FocusOnKeyEventCallback? fallback,
}) =>
    (focusNode, event) {
      if (_isSelectionCopyShortcut(event)) {
        final selection = controller.selection;
        if (selection != null && !selection.isCollapsed) {
          if (event is KeyDownEvent) {
            final text = terminal.buffer.getText(selection);
            unawaited(writeTerminalClipboard(text, userInitiated: true));
          }
          return KeyEventResult.handled;
        }
      }
      return fallback?.call(focusNode, event) ?? KeyEventResult.ignored;
    };

bool _isSelectionCopyShortcut(KeyEvent event) {
  final keyboard = HardwareKeyboard.instance;
  final platform = defaultTargetPlatform;
  final usesControlCopy =
      platform == TargetPlatform.windows || platform == TargetPlatform.android;
  return usesControlCopy &&
      (event is KeyDownEvent || event is KeyRepeatEvent) &&
      event.logicalKey == LogicalKeyboardKey.keyC &&
      keyboard.isControlPressed &&
      !keyboard.isShiftPressed &&
      !keyboard.isAltPressed &&
      !keyboard.isMetaPressed;
}
