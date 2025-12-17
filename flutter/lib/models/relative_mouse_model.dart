import 'dart:async';
import 'dart:convert';
import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_hbb/main.dart';
import 'package:flutter_hbb/utils/relative_mouse_accumulator.dart';
import 'package:get/get.dart';

import '../common.dart';
import '../consts.dart';
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

  // Native relative mouse mode support (macOS only)
  // Uses CGAssociateMouseAndMouseCursorPosition to lock cursor and NSEvent monitor for raw delta.
  static MethodChannel? _hostChannel;
  // The currently active model receiving native mouse delta events.
  // Note: Race condition between multiple sessions is not a concern here because
  // when relative mouse mode is active, the cursor is locked and the user cannot
  // switch to another session window. The user must first exit relative mouse mode
  // (via Ctrl+Alt+Shift+M) before they can interact with a different session.
  static RelativeMouseModel? _activeNativeModel;
  static bool _hostChannelInitialized = false;

  /// Initialize the host channel for native relative mouse mode.
  /// This should be called once when the app starts on macOS.
  static void initHostChannel() {
    if (!isMacOS) return;
    if (_hostChannelInitialized) return;
    _hostChannelInitialized = true;

    _hostChannel = const MethodChannel('org.rustdesk.rustdesk/host');
    _hostChannel!.setMethodCallHandler((call) async {
      if (call.method == 'onMouseDelta') {
        final args = call.arguments as Map<dynamic, dynamic>;
        final dx = args['dx'] as int;
        final dy = args['dy'] as int;
        _activeNativeModel?._onNativeMouseDelta(dx, dy);
      }
      return null;
    });
  }

  void _onNativeMouseDelta(int dx, int dy) {
    if (!enabled.value) return;
    // Send directly to remote without accumulator (native already provides integer deltas)
    _sendMouseMessageToSession({
      'type': 'move_relative',
      'x': '$dx',
      'y': '$dy',
    });
  }

  Future<bool> _enableNativeRelativeMouseMode() async {
    if (!isMacOS) return false;
    if (_hostChannel == null) {
      initHostChannel();
      if (_hostChannel == null) return false;
    }

    // Defensive guard: prevent overwriting an already-active native session.
    // In practice, this should not happen because when relative mouse mode is active,
    // the cursor is locked and the user cannot switch to another session window.
    // The user must first exit relative mouse mode (via Ctrl+Alt+Shift+M) before interacting
    // with a different session.
    if (_activeNativeModel != null && _activeNativeModel != this) {
      debugPrint(
          '[RelMouse] Another model already has native relative mouse mode active');
      return false;
    }

    try {
      final result =
          await _hostChannel!.invokeMethod('enableNativeRelativeMouseMode');
      if (result == true) {
        _activeNativeModel = this;
        return true;
      }
    } catch (e) {
      debugPrint('[RelMouse] Failed to enable native relative mouse mode: $e');
    }
    return false;
  }

  Future<void> _disableNativeRelativeMouseMode() async {
    if (!isMacOS) return;
    if (_hostChannel == null) return;

    try {
      await _hostChannel!.invokeMethod('disableNativeRelativeMouseMode');
    } catch (e) {
      debugPrint('[RelMouse] Failed to disable native relative mouse mode: $e');
    }
    if (_activeNativeModel == this) {
      _activeNativeModel = null;
    }
  }

  // Whether native relative mouse mode is currently active for this model
  bool get _isNativeRelativeMouseModeActive =>
      isMacOS && _activeNativeModel == this;

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

  // Track whether a recenter operation is in progress to prevent overlapping calls.
  bool _recenterInProgress = false;

  // Throttle buffer for batching mouse move messages (reduces network flooding).
  int _pendingDeltaX = 0;
  int _pendingDeltaY = 0;
  Timer? _throttleTimer;
  static const Duration _throttleInterval = Duration(milliseconds: 16);

  // Size of the remote image widget (for center calculation)
  Size? _imageWidgetSize;

  // Debounce timestamp for relative mouse mode toggle to prevent race conditions
  // between Rust rdev grab loop and Flutter keyboard handling.
  DateTime? _lastToggle;

  // Track M key down state for exit shortcut (Ctrl+Alt+Shift+M).
  // When M key down is blocked (shortcut triggered), we also need to block
  // the corresponding M key up to avoid orphan key up events being sent to remote.
  bool _exitShortcutMKeyDown = false;

  // Callback to cancel external throttle timer when relative mouse mode is disabled.
  VoidCallback? onDisabled;

  bool get isSupported {
    // On Linux/Wayland, cursor warping is not supported, hide the option entirely.
    if (isDesktop && isLinux && bind.mainCurrentIsWayland()) {
      return false;
    }
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
    required bool altPressed,
    required bool commandPressed,
  }) {
    // Exit shortcut: Ctrl+Alt+Shift+M (all platforms) exits relative mouse mode.
    // This combination is chosen to avoid conflicts with common application shortcuts.
    if (e.logicalKey == LogicalKeyboardKey.keyM) {
      if (isDesktop && enabled.value) {
        // Block M key up if M key down was blocked (to avoid orphan key up event on remote).
        // This must be checked before clearing the flag below.
        if (e is KeyUpEvent && _exitShortcutMKeyDown) {
          _exitShortcutMKeyDown = false;
          return true;
        }

        if (e is KeyDownEvent && ctrlPressed && altPressed && shiftPressed) {
          _exitShortcutMKeyDown = true;
          setRelativeMouseMode(false);
          return true;
        }
      }
    }

    // Toggle shortcut: Ctrl+Shift+G (Windows/Linux) or Cmd+Shift+G (macOS) toggles relative mouse mode.
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

  /// Handle raw key events for relative mouse mode.
  /// Returns true if the event was handled and should not be forwarded.
  bool handleRawKeyEvent(RawKeyEvent e) {
    // Exit shortcut: Ctrl+Alt+Shift+M (all platforms) exits relative mouse mode.
    if (e.logicalKey == LogicalKeyboardKey.keyM) {
      if (isDesktop && enabled.value) {
        // Block M key up if M key down was blocked (to avoid orphan key up event on remote).
        // This must be checked before clearing the flag below.
        if (e is RawKeyUpEvent && _exitShortcutMKeyDown) {
          _exitShortcutMKeyDown = false;
          return true;
        }

        if (e is RawKeyDownEvent) {
          final modifiers = e.data;
          final ctrlPressed = modifiers.isControlPressed;
          final altPressed = modifiers.isAltPressed;
          final shiftPressed = modifiers.isShiftPressed;
          if (ctrlPressed && altPressed && shiftPressed) {
            _exitShortcutMKeyDown = true;
            setRelativeMouseMode(false);
            return true;
          }
        }
      }
    }
    return false;
  }

  void onEnterOrLeaveImage(bool enter) {
    if (!enabled.value) return;

    // Keep the shared pointer-in-image flag in sync.
    setPointerInsideImage(enter);

    // macOS native mode: cursor is locked by CGAssociateMouseAndMouseCursorPosition,
    // no need for recenter logic. Just handle cursor visibility.
    if (_isNativeRelativeMouseModeActive) {
      if (enter) {
        _hideOsCursor();
      } else {
        _showOsCursor();
      }
      return;
    }

    if (!enter) {
      _showOsCursor();
      _releaseCursorClip();
      return;
    }

    _hideOsCursor();
    // Windows: clip cursor to window rect
    // Linux: use recenter method
    updatePointerLockCenter().then((_) {
      _recenterMouse();
    });
  }

  void onWindowBlur() {
    if (!enabled.value) return;

    // Focus can change while the pointer is outside the window (e.g. taskbar activation).
    // Do not rely on the previous "pointer inside" state across focus boundaries.
    setPointerInsideImage(false);
    _showOsCursor();
    // macOS native mode: don't call _releaseCursorClip as it would break CGAssociateMouseAndMouseCursorPosition
    if (!_isNativeRelativeMouseModeActive) {
      _releaseCursorClip();
    }
  }

  void onWindowFocus() {
    if (!enabled.value) return;

    // macOS native mode: cursor is already locked, just need to handle visibility
    if (_isNativeRelativeMouseModeActive) {
      setPointerInsideImage(false);
      _showOsCursor();
      return;
    }

    // Guard: image widget size must be available for proper center calculation.
    if (_imageWidgetSize == null) {
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
      return;
    }
    _lastToggle = now;
    setRelativeMouseMode(!enabled.value);
  }

  bool setRelativeMouseMode(bool value) {
    // Web is not supported due to Pointer Lock API integration complexity with Flutter's input system
    if (isWeb) return false;

    // Check keyboard permission before enabling - relative mouse mode requires keyboard control.
    if (value && !keyboardPerm()) {
      return false;
    }

    // On Linux/Wayland, OS-level cursor warping is not reliable with our current implementation.
    if (value && isDesktop && isLinux && bind.mainCurrentIsWayland()) {
      showToast(translate('rel-mouse-not-supported-wayland-tip'));
      return false;
    }

    // Check server version support before enabling.
    if (value && !isSupported) {
      showToast(translate('rel-mouse-not-supported-peer-tip'));
      return false;
    }

    // Desktop only: Ensure image widget size is available for proper center calculation.
    if (value && isDesktop && _imageWidgetSize == null) {
      showToast(translate('rel-mouse-not-ready-tip'));
      return false;
    }

    if (value) {
      try {
        enabled.value = true;

        // Show toast notification so user knows how to exit relative mouse mode (desktop only).
        if (isDesktop) {
          showToast(translate('rel-mouse-entered-tip'),
              alignment: Alignment.center);
        }

        // Best-effort marker for Rust rdev grab loop (ESC behavior) and peer/server state.
        // This uses a no-op delta so it does not move the remote cursor.
        // Intentionally fire-and-forget: we don't block enabling on this marker message.
        // Failures are logged but do not disable relative mouse mode.
        _sendMouseMessageToSession(
          {
            'relative_mouse_mode': '1',
            'type': 'move_relative',
            'x': '0',
            'y': '0',
          },
          disableRelativeOnError: false,
        ).catchError((e) {
          return false; // Swallow error, return type matches Future<bool>
        });

        // Desktop only: cursor manipulation
        if (isDesktop) {
          if (isMacOS) {
            // macOS: Use native relative mouse mode with CGAssociateMouseAndMouseCursorPosition
            // This locks the cursor in place and provides raw delta via NSEvent monitor.
            _enableNativeRelativeMouseMode().then((success) {
              if (!success) {
                _disableWithCleanup();
              } else {
                // Hide cursor after native mode is enabled
                _hideOsCursor();
              }
            });
          } else {
            // Windows/Linux: Use Flutter-based cursor recenter approach
            if (getPointerInsideImage()) {
              _hideOsCursor();
            } else {
              _showOsCursor();
              _releaseCursorClip();
            }

            updatePointerLockCenter()
                .then((_) => _recenterMouse())
                .catchError((e) {
              _disableWithCleanup();
            });
          }
        }
      } catch (e) {
        _disableWithCleanup();
        return false;
      }
    } else {
      // Best-effort marker for Rust rdev grab loop (ESC behavior).
      // Bypass keyboardPerm check to ensure Rust state is always synced,
      // even if permission was revoked while relative mode was active.
      _sendMouseMessageToSession(
        {
          'relative_mouse_mode': '0',
        },
        disableRelativeOnError: false,
        bypassKeyboardPerm: true,
      );

      // Desktop only: cursor manipulation
      if (isDesktop) {
        if (isMacOS) {
          // macOS: Disable native relative mouse mode
          // This already calls CGAssociateMouseAndMouseCursorPosition(1) to re-associate mouse
          _disableNativeRelativeMouseMode();
        } else {
          _releaseCursorClip();
        }
        _showOsCursor();
      }
      enabled.value = false;
      _resetState();
      onDisabled?.call();
    }

    return true;
  }

  // Flag to skip the first mouse move event after recenter (it's the recenter itself).
  bool _skipNextMouseMove = false;

  /// Handle relative mouse movement based on current local pointer position.
  /// Returns true if the event was handled in relative mode, false otherwise.
  bool handleRelativeMouseMove(Offset localPosition) {
    if (!enabled.value) return false;

    // macOS: Native mode handles delta via callback, skip Flutter-based handling.
    if (_isNativeRelativeMouseModeActive) {
      return true;
    }

    // Pointer move/hover implies we're inside the remote image.
    _ensurePointerLockEngaged();

    // Skip the mouse move event triggered by recenter operation itself.
    if (_skipNextMouseMove) {
      _skipNextMouseMove = false;
      _lastPointerLocalPos = localPosition;
      return true;
    }

    final lastLocal = _lastPointerLocalPos;
    _lastPointerLocalPos = localPosition;

    // Calculate delta from last position (not from center).
    // This avoids issues with CGWarpMouseCursorPosition integer rounding.
    if (lastLocal != null) {
      final delta = localPosition - lastLocal;
      if (delta.dx != 0 || delta.dy != 0) {
        sendRelativeMouseMove(delta.dx, delta.dy);
      }
    }

    return true;
  }

  void sendRelativeMouseMove(double dx, double dy) {
    if (!isDesktop) return;

    final delta = _accumulator.add(dx, dy, maxDelta: kMaxRelativeMouseDelta);
    if (delta == null) return;

    // Buffer the delta for throttled sending.
    _pendingDeltaX += delta.x;
    _pendingDeltaY += delta.y;

    // Start or refresh the throttle timer.
    if (_throttleTimer == null || !_throttleTimer!.isActive) {
      _throttleTimer = Timer(_throttleInterval, () => _flushPendingDelta());
    }
  }

  Future<void> _flushPendingDelta() async {
    if (!isDesktop) return;
    if (_pendingDeltaX == 0 && _pendingDeltaY == 0) return;

    final x = _pendingDeltaX;
    final y = _pendingDeltaY;
    _pendingDeltaX = 0;
    _pendingDeltaY = 0;

    final ok = await _sendMouseMessageToSession({
      'type': 'move_relative',
      'x': '$x',
      'y': '$y',
    });
    if (!ok) return;

    // Only recenter when mouse is near the edge of the image widget.
    // This allows smooth mouse movement without constant recentering.
    _recenterIfNearEdge();
  }

  // Edge threshold parameters for recenter detection.
  // Threshold is calculated as: min(maxThreshold, min(width, height) * fraction)
  static const double _edgeThresholdFraction = 0.1; // 10% of smaller dimension
  static const double _edgeThresholdMax =
      100.0; // Maximum threshold in logical pixels
  static const double _edgeThresholdMin =
      20.0; // Minimum threshold for very small widgets

  /// Calculate dynamic edge threshold based on widget size.
  double _calculateEdgeThreshold(Size size) {
    final smallerDimension = math.min(size.width, size.height);
    final dynamicThreshold = smallerDimension * _edgeThresholdFraction;
    // Clamp between min and max thresholds
    return dynamicThreshold.clamp(_edgeThresholdMin, _edgeThresholdMax);
  }

  /// Recenter the cursor only if it's near the edge of the image widget.
  void _recenterIfNearEdge() {
    final lastPos = _lastPointerLocalPos;
    final size = _imageWidgetSize;
    if (lastPos == null || size == null) return;

    // Dynamic threshold based on widget size
    final edgeThreshold = _calculateEdgeThreshold(size);

    final nearLeft = lastPos.dx < edgeThreshold;
    final nearRight = lastPos.dx > size.width - edgeThreshold;
    final nearTop = lastPos.dy < edgeThreshold;
    final nearBottom = lastPos.dy > size.height - edgeThreshold;

    if (nearLeft || nearRight || nearTop || nearBottom) {
      _recenterMouse();
    }
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

    final buttons = mouseButtonsToPeer(rawButtons);
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
    bool bypassKeyboardPerm = false,
  }) async {
    if (!bypassKeyboardPerm && !keyboardPerm()) return false;
    if (isViewCamera()) return false;

    try {
      await bind.sessionSendMouse(
        sessionId: sessionId,
        msg: json.encode(modify(msg)),
      );
      return true;
    } catch (e) {
      if (disableRelativeOnError && enabled.value) {
        _disableWithCleanup();
      }
      return false;
    }
  }

  /// Retry parameters for cursor re-centering.
  static const int _recenterMaxRetries = 3;
  static const Duration _recenterRetryDelay = Duration(milliseconds: 100);

  /// Recenter the cursor to the pointer lock center.
  /// Fire-and-forget safe: prevents overlapping calls and catches errors internally.
  Future<void> _recenterMouse() async {
    // Prevent overlapping recenter operations under high-frequency mouse moves.
    if (_recenterInProgress) return;
    _recenterInProgress = true;

    try {
      if (!enabled.value) return;
      if (!getPointerInsideImage()) return;

      final center = _pointerLockCenterScreen;
      if (center == null) {
        return;
      }

      for (int attempt = 0; attempt < _recenterMaxRetries; attempt++) {
        // Check preconditions before each attempt.
        if (!enabled.value || !getPointerInsideImage()) return;

        final ok = bind.mainSetCursorPosition(
          x: center.dx.toInt(),
          y: center.dy.toInt(),
        );
        if (ok) {
          // Skip the next mouse move event - it's triggered by the recenter itself.
          _skipNextMouseMove = true;
          return;
        }

        // Wait before retrying (except on the last attempt).
        if (attempt < _recenterMaxRetries - 1) {
          await Future.delayed(_recenterRetryDelay);
        }
      }

      // All attempts failed.
      _disableWithCleanup();
      showToast(translate('rel-mouse-lock-failed-tip'));
    } catch (e, st) {
      debugPrint('[RelMouse] Unexpected error in _recenterMouse: $e\n$st');
    } finally {
      _recenterInProgress = false;
    }
  }

  Future<void> updatePointerLockCenter({Offset? localCenter}) async {
    if (!isDesktop) return;

    // Null safety check for kWindowId.
    if (kWindowId == null) {
      if (enabled.value) {
        _disableWithCleanup();
      }
      return;
    }

    try {
      final wc = WindowController.fromWindowId(kWindowId!);
      final frame = await wc.getFrame();

      if (frame.width <= 0 || frame.height <= 0) {
        if (enabled.value) {
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
        if (enabled.value) {
          _disableWithCleanup();
        }
        return;
      }

      // Calculate screen coordinates for OS cursor positioning.
      // Use PlatformDispatcher instead of deprecated ui.window.
      final view = ui.PlatformDispatcher.instance.views.firstOrNull;
      final scale = view?.devicePixelRatio ?? 1.0;

      if (_pointerRegionTopLeftGlobal != null && scale > 0) {
        // On macOS, window frame and CGWarpMouseCursorPosition use points (not pixels).
        // On Windows, they use pixels.
        // Flutter's logical coordinates are in points on macOS.
        final centerInView =
            _pointerRegionTopLeftGlobal! + _pointerLockCenterLocal!;

        // Calculate client area offset (excluding title bar and borders)
        final clientPhysical = view?.physicalSize ?? ui.Size.zero;

        // macOS: Window frame and CGWarpMouseCursorPosition both use points (not pixels).
        // We convert clientPhysical (pixels) to points via `/ scale` to compute titleBarHeight,
        // which is the difference between the total window height and the Flutter view height.
        if (isMacOS) {
          final clientHeightPoints = clientPhysical.height / scale;
          final titleBarHeight = frame.height - clientHeightPoints;

          _pointerLockCenterScreen = Offset(
            frame.left + centerInView.dx,
            frame.top + titleBarHeight + centerInView.dy,
          );
        } else {
          // Windows/Linux: Use pixel coordinates. We estimate the client-area offset using
          // a heuristic based on the difference between frame size and client physical size.
          // This assumes symmetric horizontal borders (extraW / 2) and that the remaining
          // vertical space (extraH - borderBottom) is the title bar height.
          // Limitation: This heuristic may be inaccurate for maximized windows, custom window
          // decorations, or when the OS uses different border styles.
          // TODO: Replace this heuristic with platform API calls (e.g., GetClientRect on Windows)
          //       if precise client-area offsets are required.
          final extraW = frame.width - clientPhysical.width;
          final extraH = frame.height - clientPhysical.height;
          final borderX = extraW > 0 ? extraW / 2 : 0.0;
          final borderBottom = borderX;
          final borderTop = extraH > borderBottom ? extraH - borderBottom : 0.0;
          final clientTopLeftScreen =
              Offset(frame.left + borderX, frame.top + borderTop);

          _pointerLockCenterScreen = Offset(
            clientTopLeftScreen.dx + centerInView.dx * scale,
            clientTopLeftScreen.dy + centerInView.dy * scale,
          );
        }
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
      // macOS: no clip_cursor (CGAssociateMouseAndMouseCursorPosition stops mouse events)
      // Instead, we use recenter method like other platforms.
    } catch (e) {
      if (enabled.value) {
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

    final needsCenter =
        _pointerLockCenterLocal == null || _pointerLockCenterScreen == null;
    // Windows only: cursor clip
    final needsClip = isWindows && !_cursorClipApplied;
    if (needsCenter || needsClip) {
      updatePointerLockCenter()
          .then((_) => _recenterMouse())
          .catchError((Object e, StackTrace st) {
        debugPrint('[RelMouse] updatePointerLockCenter failed: $e\n$st');
        _disableWithCleanup();
      });
    }
  }

  void _applyCursorClipForFrame(Rect frame) {
    if (!isWindows) return;

    // Use PlatformDispatcher to get the device pixel ratio for proper scaling.
    final view = ui.PlatformDispatcher.instance.views.firstOrNull;
    final scale = view?.devicePixelRatio ?? 1.0;

    // Get the Flutter view's physical size (client area in pixels).
    final clientPhysical = view?.physicalSize ?? ui.Size.zero;

    // Calculate the non-client area (OS window title bar, borders).
    // frame includes the entire window (title bar + borders + client area).
    final extraW = frame.width - clientPhysical.width;
    final extraH = frame.height - clientPhysical.height;

    // Assume symmetric horizontal borders.
    final borderX = extraW > 0 ? extraW / 2 : 0.0;
    // Bottom border is typically the same as side borders.
    final borderBottom = borderX;
    // OS window title bar height is the remaining vertical non-client space.
    final borderTop = extraH > borderBottom ? extraH - borderBottom : 0.0;

    // Calculate client area top-left in screen coordinates.
    final clientTopLeftScreen =
        Offset(frame.left + borderX, frame.top + borderTop);

    int left, top, right, bottom;

    // If we have precise image widget info, clip to the remote image area.
    // This excludes the Flutter app's internal title bar and toolbar.
    if (_pointerRegionTopLeftGlobal != null &&
        _imageWidgetSize != null &&
        scale > 0) {
      // _pointerRegionTopLeftGlobal is in Flutter logical coordinates (relative to client area).
      // Convert to screen physical coordinates.
      left = (clientTopLeftScreen.dx + _pointerRegionTopLeftGlobal!.dx * scale)
          .toInt();
      top = (clientTopLeftScreen.dy + _pointerRegionTopLeftGlobal!.dy * scale)
          .toInt();
      right = (left + _imageWidgetSize!.width * scale).toInt();
      bottom = (top + _imageWidgetSize!.height * scale).toInt();
    } else {
      // Fallback: clip to client area (excluding OS window decorations).
      left = clientTopLeftScreen.dx.toInt();
      top = clientTopLeftScreen.dy.toInt();
      right = (frame.left + frame.width - borderX).toInt();
      bottom = (frame.top + frame.height - borderBottom).toInt();
    }

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
    if (_cursorHideCount <= 0) return;

    if (isWindows) {
      // Windows uses an internal display counter for cursor visibility.
      // We need to call ShowCursor(TRUE) repeatedly to match previous hide calls.
      while (_cursorHideCount > 0) {
        bind.mainShowCursor(show: true);
        _cursorHideCount--;
      }
    } else {
      // On other platforms, a single call is sufficient.
      bind.mainShowCursor(show: true);
      _cursorHideCount = 0;
    }
  }

  void _resetState() {
    _accumulator.reset();
    _throttleTimer?.cancel();
    _throttleTimer = null;
    _pendingDeltaX = 0;
    _pendingDeltaY = 0;
    _pointerLockCenterLocal = null;
    _pointerLockCenterScreen = null;
    _pointerRegionTopLeftGlobal = null;
    _lastPointerLocalPos = null;
    _skipNextMouseMove = false;
    setPointerInsideImage(false);
    _cursorClipApplied = false;
    _exitShortcutMKeyDown = false;
  }

  /// Core cleanup logic shared by [_disableWithCleanup] and [dispose].
  /// Sends disable message to Rust, releases platform resources, and resets state.
  void _performCleanupCore() {
    // Best-effort marker for Rust rdev grab loop (ESC behavior).
    // Bypass keyboardPerm check to ensure Rust state is always synced.
    _sendMouseMessageToSession(
      {
        'relative_mouse_mode': '0',
      },
      disableRelativeOnError: false,
      bypassKeyboardPerm: true,
    );

    // macOS: Disable native relative mouse mode
    // This already calls CGAssociateMouseAndMouseCursorPosition(1) to re-associate mouse
    if (isMacOS) {
      _disableNativeRelativeMouseMode();
    } else {
      _releaseCursorClip();
    }

    _forceRestoreCursor();
    _resetState();
  }

  void _disableWithCleanup() {
    _performCleanupCore();
    enabled.value = false;
    onDisabled?.call();
  }

  bool _disposed = false;

  void dispose() {
    if (_disposed) return;
    _disposed = true;

    _performCleanupCore();
    _imageWidgetSize = null;
    _lastToggle = null;
    onDisabled = null;
    enabled.value = false;
  }
}
