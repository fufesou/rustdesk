import 'dart:async';
import 'dart:convert';
import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/main.dart';
import 'package:xterm/xterm.dart';

import 'model.dart';
import 'platform_model.dart';

class TerminalModel with ChangeNotifier {
  final String id; // peer id
  final FFI parent;
  final int terminalId;
  late final Terminal terminal;
  late final TerminalController terminalController;

  bool _terminalOpened = false;
  bool get terminalOpened => _terminalOpened;

  bool _disposed = false;

  final _inputBuffer = <String>[];
  // Buffer for output data received before terminal view has valid dimensions.
  // This prevents NaN errors when writing to terminal before layout is complete.
  // Uses List<String> chunks to avoid truncating ANSI escape sequences mid-stream.
  final _pendingOutputChunks = <String>[];
  int _pendingOutputSize = 0;  // in characters (UTF-16 code units)
  // Local buffer limit (characters) - aligned with server's request buffer size.
  // Note: UTF-8 bytes on server vs UTF-16 chars here, but close enough for buffering purposes.
  static const int _kMaxOutputBufferChars = 8 * 1024;
  // Max bytes to request from server when reconnecting to a persistent session.
  static const int _kMaxRequestBufferBytes = 8 * 1024;
  // View ready state: true when terminal has valid dimensions, safe to write
  bool _terminalViewReady = false;

  bool get isPeerWindows => parent.ffiModel.pi.platform == kPeerPlatformWindows;

  void Function(int w, int h, int pw, int ph)? onResizeExternal;

  Future<void> _handleInput(String data) async {
    // If we press the `Enter` button on Android,
    // `data` can be '\r' or '\n' when using different keyboards.
    // Android -> Windows. '\r' works, but '\n' does not. '\n' is just a newline.
    // Android -> Linux. Both '\r' and '\n' work as expected (execute a command).
    // So when we receive '\n', we may need to convert it to '\r' to ensure compatibility.
    // Desktop -> Desktop works fine.
    // Check if we are on mobile or web(mobile), and convert '\n' to '\r'.
    final isMobileOrWebMobile = (isMobile || (isWeb && !isWebDesktop));
    if (isMobileOrWebMobile && isPeerWindows && data == '\n') {
      data = '\r';
    }
    if (_terminalOpened) {
      // Send user input to remote terminal
      try {
        await bind.sessionSendTerminalInput(
          sessionId: parent.sessionId,
          terminalId: terminalId,
          data: data,
        );
      } catch (e) {
        debugPrint('[TerminalModel] Error sending terminal input: $e');
      }
    } else {
      debugPrint('[TerminalModel] Terminal not opened yet, buffering input');
      _inputBuffer.add(data);
    }
  }

  TerminalModel(this.parent, [this.terminalId = 0]) : id = parent.id {
    terminal = Terminal(maxLines: 10000);
    terminalController = TerminalController();

    // Setup terminal callbacks
    terminal.onOutput = _handleInput;

    terminal.onResize = (w, h, pw, ph) async {
      // Validate all dimensions before using them
      if (w > 0 && h > 0 && pw > 0 && ph > 0) {
        debugPrint(
            '[TerminalModel] Terminal resized to ${w}x$h (pixel: ${pw}x$ph)');

        // Mark terminal view as ready and flush any buffered output on first valid resize.
        if (!_terminalViewReady) {
          _markViewReady();
        }

        // This piece of code must be placed before the conditional check in order to initialize properly.
        onResizeExternal?.call(w, h, pw, ph);

        if (_terminalOpened) {
          // Notify remote terminal of resize
          try {
            await bind.sessionResizeTerminal(
              sessionId: parent.sessionId,
              terminalId: terminalId,
              rows: h,
              cols: w,
            );
          } catch (e) {
            debugPrint('[TerminalModel] Error resizing terminal: $e');
          }
        }
      } else {
        debugPrint(
            '[TerminalModel] Invalid terminal dimensions: ${w}x$h (pixel: ${pw}x$ph)');
      }
    };
  }

  void onReady() {
    parent.dialogManager.dismissAll();

    // Fire and forget - don't block onReady
    openTerminal().catchError((e) {
      debugPrint('[TerminalModel] Error opening terminal: $e');
    });
  }

  Future<void> openTerminal() async {
    if (_terminalOpened) return;
    // Request the remote side to open a terminal with default shell
    // The remote side will decide which shell to use based on its OS

    // Get terminal dimensions, ensuring they are valid
    int rows = 24;
    int cols = 80;

    if (terminal.viewHeight > 0) {
      rows = terminal.viewHeight;
    }
    if (terminal.viewWidth > 0) {
      cols = terminal.viewWidth;
    }

    debugPrint(
        '[TerminalModel] Opening terminal $terminalId, sessionId: ${parent.sessionId}, size: ${cols}x$rows');
    try {
      await bind
          .sessionOpenTerminal(
        sessionId: parent.sessionId,
        terminalId: terminalId,
        rows: rows,
        cols: cols,
      )
          .timeout(
        const Duration(seconds: 5),
        onTimeout: () {
          throw TimeoutException(
              'sessionOpenTerminal timed out after 5 seconds');
        },
      );
      debugPrint('[TerminalModel] sessionOpenTerminal called successfully');
    } catch (e) {
      debugPrint('[TerminalModel] Error calling sessionOpenTerminal: $e');
      // Optionally show error to user
      if (e is TimeoutException) {
        terminal.write('Failed to open terminal: Connection timeout\r\n');
      }
    }
  }

  Future<void> sendVirtualKey(String data) async {
    return _handleInput(data);
  }

  Future<void> closeTerminal() async {
    if (_terminalOpened) {
      try {
        await bind
            .sessionCloseTerminal(
          sessionId: parent.sessionId,
          terminalId: terminalId,
        )
            .timeout(
          const Duration(seconds: 3),
          onTimeout: () {
            throw TimeoutException(
                'sessionCloseTerminal timed out after 3 seconds');
          },
        );
        debugPrint('[TerminalModel] sessionCloseTerminal called successfully');
      } catch (e) {
        debugPrint('[TerminalModel] Error calling sessionCloseTerminal: $e');
        // Continue with cleanup even if close fails
      }
      _terminalOpened = false;
      notifyListeners();
    }
  }

  static int getTerminalIdFromEvt(Map<String, dynamic> evt) {
    if (evt.containsKey('terminal_id')) {
      final v = evt['terminal_id'];
      if (v is int) {
        // Desktop and mobile send terminal_id as an int
        return v;
      } else if (v is String) {
        // Web sends terminal_id as a string
        final parsed = int.tryParse(v);
        if (parsed != null) {
          return parsed;
        } else {
          debugPrint(
              '[TerminalModel] Failed to parse terminal_id as integer: $v. Expected a numeric string.');
          return 0;
        }
      } else {
        // Unexpected type, log and handle gracefully
        debugPrint(
            '[TerminalModel] Unexpected terminal_id type: ${v.runtimeType}, value: $v. Expected int or String.');
        return 0;
      }
    } else {
      debugPrint('[TerminalModel] Event does not contain terminal_id');
      return 0;
    }
  }

  /// Parse a boolean value from event map, handling both bool and String types (for web compatibility).
  static bool getBoolFromEvt(Map<String, dynamic> evt, String key, {bool defaultValue = false}) {
    final v = evt[key];
    if (v is bool) return v;
    if (v is String) return v.toLowerCase() == 'true';
    return defaultValue;
  }

  void handleTerminalResponse(Map<String, dynamic> evt) {
    final String? type = evt['type'];
    final int evtTerminalId = getTerminalIdFromEvt(evt);

    // Only handle events for this terminal
    if (evtTerminalId != terminalId) {
      debugPrint(
          '[TerminalModel] Ignoring event for terminal $evtTerminalId (not mine)');
      return;
    }

    switch (type) {
      case 'opened':
        _handleTerminalOpened(evt);
        break;
      case 'data':
        _handleTerminalData(evt);
        break;
      case 'closed':
        _handleTerminalClosed(evt);
        break;
      case 'error':
        _handleTerminalError(evt);
        break;
    }
  }

  void _handleTerminalOpened(Map<String, dynamic> evt) {
    final bool success = getBoolFromEvt(evt, 'success');
    final String message = evt['message'] ?? '';
    final String? serviceId = evt['service_id'];
    final bool reconnected = getBoolFromEvt(evt, 'reconnected');

    debugPrint(
        '[TerminalModel] Terminal opened response: success=$success, message=$message, service_id=$serviceId, reconnected=$reconnected');

    if (success) {
      _terminalOpened = true;

      // Service ID is now saved on the Rust side in handle_terminal_response

      // If this is a reconnection to an existing terminal, request the buffer
      if (reconnected) {
        debugPrint(
            '[TerminalModel] Reconnected to existing terminal, requesting buffer');
        _requestTerminalBuffer();
      }

      // Process any buffered input
      _processBufferedInputAsync().then((_) {
        notifyListeners();
      }).catchError((e) {
        debugPrint('[TerminalModel] Error processing buffered input: $e');
        notifyListeners();
      });

      final persistentSessions =
          evt['persistent_sessions'] as List<dynamic>? ?? [];
      if (kWindowId != null && persistentSessions.isNotEmpty) {
        DesktopMultiWindow.invokeMethod(
            kWindowId!,
            kWindowEventRestoreTerminalSessions,
            jsonEncode({
              'persistent_sessions': persistentSessions,
            }));
      }
    } else {
      terminal.write('Failed to open terminal: $message\r\n');
    }
  }

  Future<void> _processBufferedInputAsync() async {
    final buffer = List<String>.from(_inputBuffer);
    _inputBuffer.clear();

    for (final data in buffer) {
      try {
        await bind.sessionSendTerminalInput(
          sessionId: parent.sessionId,
          terminalId: terminalId,
          data: data,
        );
      } catch (e) {
        debugPrint('[TerminalModel] Error sending buffered input: $e');
      }
    }
  }

  /// Mark terminal view as ready and flush any buffered output.
  /// Called on first valid resize when terminal has valid dimensions.
  void _markViewReady() {
    if (_terminalViewReady) return;
    _terminalViewReady = true;
    // Flush buffered output chunks in order
    if (_pendingOutputChunks.isNotEmpty) {
      debugPrint('[TerminalModel] Flushing ${_pendingOutputChunks.length} buffered output chunks ($_pendingOutputSize chars)');
      for (final chunk in _pendingOutputChunks) {
        terminal.write(chunk);
      }
      _pendingOutputChunks.clear();
      _pendingOutputSize = 0;
    }
  }

  /// Request terminal buffer from server (fire-and-forget with error logging).
  void _requestTerminalBuffer() {
    if (_disposed) return;
    bind.sessionRequestTerminalBuffer(
      sessionId: parent.sessionId,
      terminalId: terminalId,
      maxBytes: _kMaxRequestBufferBytes,
    ).catchError((e) {
      debugPrint('[TerminalModel] Error requesting terminal buffer: $e');
    });
  }

  void _handleTerminalData(Map<String, dynamic> evt) {
    final data = evt['data'];

    if (data != null) {
      try {
        String text = '';
        if (data is String) {
          // Try to decode as base64 first
          try {
            final bytes = base64Decode(data);
            text = utf8.decode(bytes, allowMalformed: true);
          } catch (e) {
            // If base64 decode fails, treat as plain text
            text = data;
          }
        } else if (data is List) {
          // Handle if data comes as byte array
          text = utf8.decode(List<int>.from(data), allowMalformed: true);
        } else {
          debugPrint('[TerminalModel] Unknown data type: ${data.runtimeType}');
          return;
        }

        // Buffer data if terminal view is not ready yet to avoid NaN errors.
        if (!_terminalViewReady) {
          _pendingOutputChunks.add(text);
          _pendingOutputSize += text.length;
          // Drop oldest chunks if exceeds limit (whole chunks to preserve ANSI sequences)
          while (_pendingOutputSize > _kMaxOutputBufferChars && _pendingOutputChunks.length > 1) {
            final removed = _pendingOutputChunks.removeAt(0);
            _pendingOutputSize -= removed.length;
          }
          return;
        }

        terminal.write(text);
      } catch (e) {
        debugPrint('[TerminalModel] Failed to process terminal data: $e');
      }
    }
  }

  void _handleTerminalClosed(Map<String, dynamic> evt) {
    final int exitCode = evt['exit_code'] ?? 0;
    terminal.write('\r\nTerminal closed with exit code: $exitCode\r\n');
    _terminalOpened = false;
    notifyListeners();
  }

  void _handleTerminalError(Map<String, dynamic> evt) {
    final String message = evt['message'] ?? 'Unknown error';
    terminal.write('\r\nTerminal error: $message\r\n');
  }

  @override
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    // Clear buffers to free memory
    _inputBuffer.clear();
    _pendingOutputChunks.clear();
    _pendingOutputSize = 0;
    // Terminal cleanup is handled server-side when service closes
    super.dispose();
  }
}
