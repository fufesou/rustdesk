import 'dart:convert';
import 'dart:ui' as ui;

import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_hbb/main.dart';
import 'package:flutter_hbb/utils/relative_mouse_accumulator.dart';
import 'package:get/get.dart';

import '../common.dart';
import '../consts.dart';
import 'input_model.dart';
import 'platform_model.dart';

class RelativeMouseModel {
  final SessionID sessionId;
  final RxBool enabled;

  final bool Function() keyboardPerm;
  final bool Function() isViewCamera;
  final String Function() peerVersion;

  final Map<String, dynamic> Function(Map<String, dynamic> msg) modify;

  final bool Function() getPointerInsideImage;
  final void Function(bool inside) setPointerInsideImage;

  RelativeMouseModel({
    required this.sessionId,
    required this.enabled,
    required this.keyboardPerm,
    required this.isViewCamera,
    required this.peerVersion,
    required this.modify,
    required this.getPointerInsideImage,
    required this.setPointerInsideImage,
  });

  final RelativeMouseAccumulator _accumulator = RelativeMouseAccumulator();

  // Pointer lock center in LOCAL widget coordinates (for delta calculation)
  Offset? _pointerLockCenterLocal;
  // Pointer lock center in SCREEN coordinates (for OS cursor re-centering)
  Offset? _pointerLockCenterScreen;
  // Pointer region top-left in Flutter view coordinates.
  // Computed from PointerEvent.position - PointerEvent.localPosition.
  Offset? _pointerRegionTopLeftGlobal;
  // Last pointer position in LOCAL widget coordinates (fallback when center is not ready).
  Offset? _lastPointerLocalPos;

  // Track how many times we've hidden the OS cursor (for proper reference count management on Windows).
  // Windows ShowCursor uses a reference counting mechanism - we need to match hide/show calls exactly.
  int _cursorHideCount = 0;

  // Track whether we currently have an OS-level cursor clip active (Windows only).
  bool _cursorClipApplied = false;

  // Size of the remote image widget (for center calculation)
  Size? _imageWidgetSize;

  // Debounce timestamp for relative mouse mode toggle to prevent race conditions
  // between Rust rdev grab loop and Flutter keyboard handling.
  DateTime? _lastToggle;

  // When exiting relative mouse mode via ESC, swallow the corresponding ESC key up
  // (and any repeats) so the key is not forwarded to the peer.
  bool _suppressEscapeKeyUp = false;

  // Callback to cancel external throttle timer when relative mouse mode is disabled.
  VoidCallback? onDisabled;

  bool get isSupported {
    final v = peerVersion();
    if (v.isEmpty) return false;
    return versionCmp(v, kMinVersionForRelativeMouseMode) >= 0;
  }

  bool get isToggleShortcutEnabled =>
      bind.mainGetUserDefaultOption(key: kOptionEnableRelativeMouseShortcut) ==
      'Y';

  Size? get imageWidgetSize => _imageWidgetSize;

  void updateImageWidgetSize(Size size) {
    _imageWidgetSize = size;
    if (enabled.value) {
      _pointerLockCenterLocal = Offset(size.width / 2, size.height / 2);
    }
  }

  void updatePointerRegionTopLeftGlobal(PointerEvent e) {
    _pointerRegionTopLeftGlobal = e.position - e.localPosition;
  }

  bool handleKeyEvent(
    KeyEvent e, {
    required bool ctrlPressed,
    required bool shiftPressed,
    required bool commandPressed,
  }) {
    // ESC behavior:
    // - If relative mouse mode is enabled, ESC exits relative mouse mode and must NOT be sent to peer.
    // - If relative mouse mode is not enabled, ESC should be sent to peer as a normal key.
    if (e.logicalKey == LogicalKeyboardKey.escape) {
      if (enabled.value && e is KeyDownEvent) {
        _suppressEscapeKeyUp = true;
        setRelativeMouseMode(false);
        return true;
      }

      // Swallow the corresponding key up (and repeats) after exiting via ESC.
      if (_suppressEscapeKeyUp) {
        if (e is KeyUpEvent) {
          _suppressEscapeKeyUp = false;
          return true;
        }
        if (e is KeyRepeatEvent) {
          return true;
        }
        if (e is KeyDownEvent) {
          // New ESC press without receiving the previous key up.
          _suppressEscapeKeyUp = false;
        }
      }
    }

    // Shortcut: Ctrl+Shift+G (Windows/Linux) or Cmd+Shift+G (macOS) toggles relative mouse mode.
    if (e is KeyDownEvent && e.logicalKey == LogicalKeyboardKey.keyG) {
      if (isDesktop && keyboardPerm() && !isViewCamera()) {
        final modifierPressed = isMacOS ? commandPressed : ctrlPressed;
        if (modifierPressed && shiftPressed) {
          if (!isToggleShortcutEnabled) {
            return false;
          }
          toggleRelativeMouseMode();
          return true;
        }
      }
    }

    return false;
  }

  bool handleRawKeyEvent(RawKeyEvent e) {
    if (e.logicalKey != LogicalKeyboardKey.escape) {
      return false;
    }

    if (enabled.value && e is RawKeyDownEvent) {
      _suppressEscapeKeyUp = true;
      setRelativeMouseMode(false);
      return true;
    }

    if (_suppressEscapeKeyUp) {
      if (e is RawKeyUpEvent) {
        _suppressEscapeKeyUp = false;
        return true;
      }
      if (e is RawKeyDownEvent) {
        if (e.repeat) {
          return true;
        }
        // New ESC press without receiving the previous key up.
        _suppressEscapeKeyUp = false;
      }
    }

    return false;
  }

  void onEnterOrLeaveImage(bool enter) {
    if (!enabled.value) return;

    // Keep the shared pointer-in-image flag in sync.
    setPointerInsideImage(enter);

    if (!enter) {
      _showOsCursor();
      _releaseCursorClip();
      return;
    }

    _hideOsCursor();
    updatePointerLockCenter().then((_) => _recenterMouse());
  }

  void onWindowBlur() {
    if (!enabled.value) return;

    // Focus can change while the pointer is outside the window (e.g. taskbar activation).
    // Do not rely on the previous "pointer inside" state across focus boundaries.
    setPointerInsideImage(false);
    _showOsCursor();
    _releaseCursorClip();
  }

  void onWindowFocus() {
    if (!enabled.value) return;

    // Guard: image widget size must be available for proper center calculation.
    if (_imageWidgetSize == null) {
      debugPrint('onWindowFocus: image widget size is null, disabling relative mouse mode');
      _disableWithCleanup();
      return;
    }

    // Fail-safe: keep cursor usable on focus gain. Pointer lock will be re-engaged
    // on the next pointer enter/move/hover inside the remote image.
    setPointerInsideImage(false);
    _showOsCursor();
    _releaseCursorClip();

    // Best-effort: refresh center so the next engage is immediate.
    updatePointerLockCenter();
  }

  void toggleRelativeMouseMode() {
    if (!isToggleShortcutEnabled) {
      return;
    }

    final now = DateTime.now();
    if (_lastToggle != null &&
        now.difference(_lastToggle!).inMilliseconds <
            kRelativeMouseModeToggleDebounceMs) {
      debugPrint('Ignoring relative mouse mode toggle: debounce protection');
      return;
    }
    _lastToggle = now;
    setRelativeMouseMode(!enabled.value);
  }

  bool setRelativeMouseMode(bool value) {
    if (!isDesktop) return false;

    // On Linux/Wayland, OS-level cursor warping is not reliable with our current implementation.
    if (value && isLinux && bind.mainCurrentIsWayland()) {
      showToast(translate('rel-mouse-not-supported-wayland-tip'));
      return false;
    }

    // Check server version support before enabling.
    if (value && !isSupported) {
      final v = peerVersion();
      debugPrint(
          'Relative mouse mode not supported by server version: $v (requires >= $kMinVersionForRelativeMouseMode)');
      showToast(translate('rel-mouse-not-supported-peer-tip'));
      return false;
    }

    // Ensure image widget size is available for proper center calculation.
    if (value && _imageWidgetSize == null) {
      debugPrint(
          'Relative mouse mode cannot be enabled: image widget size not yet available');
      showToast(translate('rel-mouse-not-ready-tip'));
      return false;
    }

    if (value) {
      try {
        enabled.value = true;

        // Best-effort marker for Rust rdev grab loop (ESC behavior) and peer/server state.
        // This uses a no-op delta so it does not move the remote cursor.
        _sendMouseMessageToSession(
          {
            'relative_mouse_mode': '1',
            'type': 'move_relative',
            'x': '0',
            'y': '0',
          },
          disableRelativeOnError: false,
        );

        if (getPointerInsideImage()) {
          _hideOsCursor();
        } else {
          _showOsCursor();
          _releaseCursorClip();
        }

        updatePointerLockCenter().then((_) => _recenterMouse()).catchError((e) {
          debugPrint('Failed to update pointer lock center: $e');
          _disableWithCleanup();
        });
      } catch (e) {
        debugPrint('Failed to enable relative mouse mode: $e');
        _disableWithCleanup();
        return false;
      }
    } else {
      // Best-effort marker for Rust rdev grab loop (ESC behavior).
      _sendMouseMessageToSession(
        {
          'relative_mouse_mode': '0',
        },
        disableRelativeOnError: false,
      );

      _showOsCursor();
      _releaseCursorClip();
      enabled.value = false;
      _resetState();
      onDisabled?.call();
    }

    return true;
  }

  /// Handle relative mouse movement based on current local pointer position.
  /// Returns true if the event was handled in relative mode, false otherwise.
  bool handleRelativeMouseMove(Offset localPosition) {
    if (!enabled.value) return false;

    // Pointer move/hover implies we're inside the remote image.
    _ensurePointerLockEngaged();

    final lastLocal = _lastPointerLocalPos;
    _lastPointerLocalPos = localPosition;

    final center = _pointerLockCenterLocal;
    if (center != null) {
      // Calculate delta from LOCAL CENTER position, not last position.
      // This prevents feedback loop when re-centering cursor.
      final delta = localPosition - center;
      if (delta.dx != 0 || delta.dy != 0) {
        sendRelativeMouseMove(delta.dx, delta.dy);
      }
    } else if (lastLocal != null) {
      // Fallback while center is not ready yet.
      final delta = localPosition - lastLocal;
      if (delta.dx != 0 || delta.dy != 0) {
        sendRelativeMouseMove(delta.dx, delta.dy);
      }
    }

    return true;
  }

  Future<void> sendRelativeMouseMove(double dx, double dy) async {
    if (!isDesktop) return;

    final delta = _accumulator.add(dx, dy, maxDelta: kMaxRelativeMouseDelta);
    if (delta == null) return;

    final ok = await _sendMouseMessageToSession({
      'type': 'move_relative',
      'x': '${delta.x}',
      'y': '${delta.y}',
    });
    if (!ok) return;

    _recenterMouse();
  }

  /// Send mouse button event without position (for relative mouse mode).
  Future<void> sendRelativeMouseButton(Map<String, dynamic> evt) async {
    if (!enabled.value) return;
    _ensurePointerLockEngaged();

    final rawType = evt['type'];
    final rawButtons = evt['buttons'];
    if (rawType is! String || rawButtons is! int) return;

    final type = _mouseEventTypeToPeer(rawType);
    if (type.isEmpty) return;

    final buttons = InputModel.mouseButtonsToPeer(rawButtons);
    if (buttons.isEmpty) return;

    await _sendMouseMessageToSession({
      'type': type,
      'buttons': buttons,
    });
  }

  static String _mouseEventTypeToPeer(String type) {
    switch (type) {
      case 'mousedown':
        return kMouseEventTypeDown;
      case 'mouseup':
        return kMouseEventTypeUp;
      default:
        return '';
    }
  }


  Future<bool> _sendMouseMessageToSession(
    Map<String, dynamic> msg, {
    bool disableRelativeOnError = true,
  }) async {
    if (!keyboardPerm()) return false;
    if (isViewCamera()) return false;

    try {
      await bind.sessionSendMouse(
        sessionId: sessionId,
        msg: json.encode(modify(msg)),
      );
      return true;
    } catch (e) {
      debugPrint('_sendMouseMessageToSession failed: $e');
      if (disableRelativeOnError && enabled.value) {
        _disableWithCleanup();
      }
      return false;
    }
  }

  void _recenterMouse() {
    if (!enabled.value) return;
    if (!getPointerInsideImage()) return;

    final center = _pointerLockCenterScreen;
    if (center == null) return;

    final ok = bind.mainSetCursorPosition(
      x: center.dx.toInt(),
      y: center.dy.toInt(),
    );
    if (!ok) {
      debugPrint('Failed to re-center cursor, disabling relative mouse mode');
      _disableWithCleanup();
      showToast(translate(
          'rel-mouse-lock-failed-tip'));
    }
  }

  Future<void> updatePointerLockCenter({Offset? localCenter}) async {
    if (!isDesktop) return;

    // Null safety check for kWindowId.
    if (kWindowId == null) {
      debugPrint(
          'updatePointerLockCenter: kWindowId is null, cannot update pointer lock center');
      if (enabled.value) {
        debugPrint('Disabling relative mouse mode due to missing window id');
        _disableWithCleanup();
      }
      return;
    }

    try {
      final wc = WindowController.fromWindowId(kWindowId!);
      final frame = await wc.getFrame();

      if (frame.width <= 0 || frame.height <= 0) {
        debugPrint(
            'updatePointerLockCenter: invalid window frame dimensions (${frame.width}x${frame.height})');
        if (enabled.value) {
          debugPrint('Disabling relative mouse mode due to invalid window frame');
          _disableWithCleanup();
        }
        return;
      }

      if (localCenter != null) {
        _pointerLockCenterLocal = localCenter;
      } else if (_imageWidgetSize != null) {
        _pointerLockCenterLocal = Offset(
          _imageWidgetSize!.width / 2,
          _imageWidgetSize!.height / 2,
        );
      } else {
        debugPrint(
            'updatePointerLockCenter: image widget size is null, disabling relative mouse mode');
        if (enabled.value) {
          _disableWithCleanup();
        }
        return;
      }

      // Calculate screen coordinates for OS cursor positioning.
      final scale = ui.window.devicePixelRatio;
      if (_pointerRegionTopLeftGlobal != null && scale > 0) {
        // Estimate the client-area top-left in screen coordinates.
        final clientPhysical = ui.window.physicalSize;
        final extraW = frame.width - clientPhysical.width;
        final extraH = frame.height - clientPhysical.height;
        final borderX = extraW > 0 ? extraW / 2 : 0.0;
        final borderBottom = borderX;
        final borderTop = extraH > borderBottom ? extraH - borderBottom : 0.0;
        final clientTopLeftScreen = Offset(frame.left + borderX, frame.top + borderTop);

        final centerInView = _pointerRegionTopLeftGlobal! + _pointerLockCenterLocal!;
        _pointerLockCenterScreen = Offset(
          clientTopLeftScreen.dx + centerInView.dx * scale,
          clientTopLeftScreen.dy + centerInView.dy * scale,
        );
      } else {
        _pointerLockCenterScreen = Offset(
          frame.left + frame.width / 2,
          frame.top + frame.height / 2,
        );
      }

      if (enabled.value && isWindows && getPointerInsideImage()) {
        _applyCursorClipForFrame(frame);
      } else if (enabled.value && isWindows) {
        _releaseCursorClip();
      }
    } catch (e) {
      debugPrint('Failed to get window frame for pointer lock center: $e');
      if (enabled.value) {
        debugPrint('Disabling relative mouse mode due to window frame error');
        _disableWithCleanup();
      } else {
        _pointerLockCenterLocal = null;
        _pointerLockCenterScreen = null;
      }
    }
  }

  void _ensurePointerLockEngaged() {
    if (!enabled.value) return;
    if (!isDesktop) return;

    setPointerInsideImage(true);
    _hideOsCursor();

    final needsCenter = _pointerLockCenterLocal == null || _pointerLockCenterScreen == null;
    final needsClip = isWindows && !_cursorClipApplied;
    if (needsCenter || needsClip) {
      updatePointerLockCenter().then((_) => _recenterMouse()).catchError((e) {
        debugPrint('Failed to update pointer lock center: $e');
        _disableWithCleanup();
      });
    }
  }

  void _applyCursorClipForFrame(Rect frame) {
    if (!isWindows) return;

    final left = frame.left.toInt();
    final top = frame.top.toInt();
    final right = (frame.left + frame.width).toInt();
    final bottom = (frame.top + frame.height).toInt();

    _cursorClipApplied = bind.mainClipCursor(
      left: left,
      top: top,
      right: right,
      bottom: bottom,
      enable: true,
    );
  }

  void _releaseCursorClip() {
    _cursorClipApplied = false;
    if (!isWindows) return;

    bind.mainClipCursor(
      left: 0,
      top: 0,
      right: 0,
      bottom: 0,
      enable: false,
    );
  }

  void _hideOsCursor() {
    if (_cursorHideCount == 0) {
      bind.mainShowCursor(show: false);
      _cursorHideCount = 1;
    }
  }

  void _showOsCursor() {
    if (_cursorHideCount > 0) {
      bind.mainShowCursor(show: true);
      _cursorHideCount = 0;
    }
  }

  void _forceRestoreCursor() {
    while (_cursorHideCount > 0) {
      bind.mainShowCursor(show: true);
      _cursorHideCount--;
    }
  }

  void _resetState() {
    _accumulator.reset();
    _pointerLockCenterLocal = null;
    _pointerLockCenterScreen = null;
    _pointerRegionTopLeftGlobal = null;
    _lastPointerLocalPos = null;
    setPointerInsideImage(false);
    _cursorClipApplied = false;
  }

  void _disableWithCleanup() {
    // Best-effort marker for Rust rdev grab loop (ESC behavior).
    _sendMouseMessageToSession(
      {
        'relative_mouse_mode': '0',
      },
      disableRelativeOnError: false,
    );

    _forceRestoreCursor();
    _releaseCursorClip();
    enabled.value = false;
    _resetState();
    onDisabled?.call();
  }

  void dispose() {
    // Best-effort marker for Rust rdev grab loop (ESC behavior).
    _sendMouseMessageToSession(
      {
        'relative_mouse_mode': '0',
      },
      disableRelativeOnError: false,
    );

    _forceRestoreCursor();
    _releaseCursorClip();
    _resetState();
    _imageWidgetSize = null;
    _lastToggle = null;
    onDisabled = null;
    enabled.value = false;
  }
}
