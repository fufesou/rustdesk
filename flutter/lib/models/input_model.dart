import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math';
import 'dart:ui' as ui;

import 'package:desktop_multi_window/desktop_multi_window.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_hbb/main.dart';
import 'package:flutter_hbb/utils/multi_window_manager.dart';
import 'package:flutter_hbb/utils/relative_mouse_accumulator.dart';
import 'package:get/get.dart';

import '../../models/model.dart';
import '../../models/platform_model.dart';
import '../common.dart';
import '../consts.dart';

/// Mouse button enum.
enum MouseButtons { left, right, wheel, back }

const _kMouseEventDown = 'mousedown';
const _kMouseEventUp = 'mouseup';
const _kMouseEventMove = 'mousemove';

class CanvasCoords {
  double x = 0;
  double y = 0;
  double scale = 1.0;
  double scrollX = 0;
  double scrollY = 0;
  ScrollStyle scrollStyle = ScrollStyle.scrollauto;
  Size size = Size.zero;

  CanvasCoords();

  Map<String, dynamic> toJson() {
    return {
      'x': x,
      'y': y,
      'scale': scale,
      'scrollX': scrollX,
      'scrollY': scrollY,
      'scrollStyle': scrollStyle.toJson(),
      'size': {
        'w': size.width,
        'h': size.height,
      }
    };
  }

  static CanvasCoords fromJson(Map<String, dynamic> json) {
    final model = CanvasCoords();
    model.x = json['x'];
    model.y = json['y'];
    model.scale = json['scale'];
    model.scrollX = json['scrollX'];
    model.scrollY = json['scrollY'];
    model.scrollStyle = ScrollStyle.fromJson(json['scrollStyle'], ScrollStyle.scrollauto);
    model.size = Size(json['size']['w'], json['size']['h']);
    return model;
  }

  static CanvasCoords fromCanvasModel(CanvasModel model) {
    final coords = CanvasCoords();
    coords.x = model.x;
    coords.y = model.y;
    coords.scale = model.scale;
    coords.scrollX = model.scrollX;
    coords.scrollY = model.scrollY;
    coords.scrollStyle = model.scrollStyle;
    coords.size = model.size;
    return coords;
  }
}

class CursorCoords {
  Offset offset = Offset.zero;

  CursorCoords();

  Map<String, dynamic> toJson() {
    return {
      'offset_x': offset.dx,
      'offset_y': offset.dy,
    };
  }

  static CursorCoords fromJson(Map<String, dynamic> json) {
    final model = CursorCoords();
    model.offset = Offset(json['offset_x'], json['offset_y']);
    return model;
  }

  static CursorCoords fromCursorModel(CursorModel model) {
    final coords = CursorCoords();
    coords.offset = model.offset;
    return coords;
  }
}

class RemoteWindowCoords {
  RemoteWindowCoords(
      this.windowRect, this.canvas, this.cursor, this.remoteRect);
  Rect windowRect;
  CanvasCoords canvas;
  CursorCoords cursor;
  Rect remoteRect;
  Offset relativeOffset = Offset.zero;

  Map<String, dynamic> toJson() {
    return {
      'canvas': canvas.toJson(),
      'cursor': cursor.toJson(),
      'windowRect': rectToJson(windowRect),
      'remoteRect': rectToJson(remoteRect),
    };
  }

  static Map<String, dynamic> rectToJson(Rect r) {
    return {
      'l': r.left,
      't': r.top,
      'w': r.width,
      'h': r.height,
    };
  }

  static Rect rectFromJson(Map<String, dynamic> json) {
    return Rect.fromLTWH(
      json['l'],
      json['t'],
      json['w'],
      json['h'],
    );
  }

  RemoteWindowCoords.fromJson(Map<String, dynamic> json)
      : windowRect = rectFromJson(json['windowRect']),
        canvas = CanvasCoords.fromJson(json['canvas']),
        cursor = CursorCoords.fromJson(json['cursor']),
        remoteRect = rectFromJson(json['remoteRect']);
}

extension ToString on MouseButtons {
  String get value {
    switch (this) {
      case MouseButtons.left:
        return 'left';
      case MouseButtons.right:
        return 'right';
      case MouseButtons.wheel:
        return 'wheel';
      case MouseButtons.back:
        return 'back';
    }
  }
}

class PointerEventToRust {
  final String kind;
  final String type;
  final dynamic value;

  PointerEventToRust(this.kind, this.type, this.value);

  Map<String, dynamic> toJson() {
    return {
      'k': kind,
      'v': {
        't': type,
        'v': value,
      }
    };
  }
}

class ToReleaseRawKeys {
  RawKeyEvent? lastLShiftKeyEvent;
  RawKeyEvent? lastRShiftKeyEvent;
  RawKeyEvent? lastLCtrlKeyEvent;
  RawKeyEvent? lastRCtrlKeyEvent;
  RawKeyEvent? lastLAltKeyEvent;
  RawKeyEvent? lastRAltKeyEvent;
  RawKeyEvent? lastLCommandKeyEvent;
  RawKeyEvent? lastRCommandKeyEvent;
  RawKeyEvent? lastSuperKeyEvent;

  reset() {
    lastLShiftKeyEvent = null;
    lastRShiftKeyEvent = null;
    lastLCtrlKeyEvent = null;
    lastRCtrlKeyEvent = null;
    lastLAltKeyEvent = null;
    lastRAltKeyEvent = null;
    lastLCommandKeyEvent = null;
    lastRCommandKeyEvent = null;
    lastSuperKeyEvent = null;
  }

  updateKeyDown(LogicalKeyboardKey logicKey, RawKeyDownEvent e) {
    if (e.isAltPressed) {
      if (logicKey == LogicalKeyboardKey.altLeft) {
        lastLAltKeyEvent = e;
      } else if (logicKey == LogicalKeyboardKey.altRight) {
        lastRAltKeyEvent = e;
      }
    } else if (e.isControlPressed) {
      if (logicKey == LogicalKeyboardKey.controlLeft) {
        lastLCtrlKeyEvent = e;
      } else if (logicKey == LogicalKeyboardKey.controlRight) {
        lastRCtrlKeyEvent = e;
      }
    } else if (e.isShiftPressed) {
      if (logicKey == LogicalKeyboardKey.shiftLeft) {
        lastLShiftKeyEvent = e;
      } else if (logicKey == LogicalKeyboardKey.shiftRight) {
        lastRShiftKeyEvent = e;
      }
    } else if (e.isMetaPressed) {
      if (logicKey == LogicalKeyboardKey.metaLeft) {
        lastLCommandKeyEvent = e;
      } else if (logicKey == LogicalKeyboardKey.metaRight) {
        lastRCommandKeyEvent = e;
      } else if (logicKey == LogicalKeyboardKey.superKey) {
        lastSuperKeyEvent = e;
      }
    }
  }

  updateKeyUp(LogicalKeyboardKey logicKey, RawKeyUpEvent e) {
    if (e.isAltPressed) {
      if (logicKey == LogicalKeyboardKey.altLeft) {
        lastLAltKeyEvent = null;
      } else if (logicKey == LogicalKeyboardKey.altRight) {
        lastRAltKeyEvent = null;
      }
    } else if (e.isControlPressed) {
      if (logicKey == LogicalKeyboardKey.controlLeft) {
        lastLCtrlKeyEvent = null;
      } else if (logicKey == LogicalKeyboardKey.controlRight) {
        lastRCtrlKeyEvent = null;
      }
    } else if (e.isShiftPressed) {
      if (logicKey == LogicalKeyboardKey.shiftLeft) {
        lastLShiftKeyEvent = null;
      } else if (logicKey == LogicalKeyboardKey.shiftRight) {
        lastRShiftKeyEvent = null;
      }
    } else if (e.isMetaPressed) {
      if (logicKey == LogicalKeyboardKey.metaLeft) {
        lastLCommandKeyEvent = null;
      } else if (logicKey == LogicalKeyboardKey.metaRight) {
        lastRCommandKeyEvent = null;
      } else if (logicKey == LogicalKeyboardKey.superKey) {
        lastSuperKeyEvent = null;
      }
    }
  }

  release(KeyEventResult Function(RawKeyEvent e) handleRawKeyEvent) {
    for (final key in [
      lastLShiftKeyEvent,
      lastRShiftKeyEvent,
      lastLCtrlKeyEvent,
      lastRCtrlKeyEvent,
      lastLAltKeyEvent,
      lastRAltKeyEvent,
      lastLCommandKeyEvent,
      lastRCommandKeyEvent,
      lastSuperKeyEvent,
    ]) {
      if (key != null) {
        handleRawKeyEvent(RawKeyUpEvent(
          data: key.data,
          character: key.character,
        ));
      }
    }
  }
}

class ToReleaseKeys {
  KeyEvent? lastLShiftKeyEvent;
  KeyEvent? lastRShiftKeyEvent;
  KeyEvent? lastLCtrlKeyEvent;
  KeyEvent? lastRCtrlKeyEvent;
  KeyEvent? lastLAltKeyEvent;
  KeyEvent? lastRAltKeyEvent;
  KeyEvent? lastLCommandKeyEvent;
  KeyEvent? lastRCommandKeyEvent;
  KeyEvent? lastSuperKeyEvent;

  reset() {
    lastLShiftKeyEvent = null;
    lastRShiftKeyEvent = null;
    lastLCtrlKeyEvent = null;
    lastRCtrlKeyEvent = null;
    lastLAltKeyEvent = null;
    lastRAltKeyEvent = null;
    lastLCommandKeyEvent = null;
    lastRCommandKeyEvent = null;
    lastSuperKeyEvent = null;
  }

  release(KeyEventResult Function(KeyEvent e) handleKeyEvent) {
    for (final key in [
      lastLShiftKeyEvent,
      lastRShiftKeyEvent,
      lastLCtrlKeyEvent,
      lastRCtrlKeyEvent,
      lastLAltKeyEvent,
      lastRAltKeyEvent,
      lastLCommandKeyEvent,
      lastRCommandKeyEvent,
      lastSuperKeyEvent,
    ]) {
      if (key != null) {
        handleKeyEvent(key);
      }
    }
  }
}

class InputModel {
  final WeakReference<FFI> parent;
  String keyboardMode = '';

  // keyboard
  var shift = false;
  var ctrl = false;
  var alt = false;
  var command = false;

  final ToReleaseRawKeys toReleaseRawKeys = ToReleaseRawKeys();
  final ToReleaseKeys toReleaseKeys = ToReleaseKeys();

  // trackpad
  var _trackpadLastDelta = Offset.zero;
  var _stopFling = true;
  var _fling = false;
  Timer? _flingTimer;
  final _flingBaseDelay = 30;
  final _trackpadAdjustPeerLinux = 0.06;
  // This is an experience value.
  final _trackpadAdjustMacToWin = 2.50;
  int _trackpadSpeed = kDefaultTrackpadSpeed;
  double _trackpadSpeedInner = kDefaultTrackpadSpeed / 100.0;
  var _trackpadScrollUnsent = Offset.zero;

  var _lastScale = 1.0;

  bool _pointerMovedAfterEnter = false;
  bool _pointerInsideImage = false;

  // mouse
  final isPhysicalMouse = false.obs;
  int _lastButtons = 0;
  Offset lastMousePos = Offset.zero;

  // Relative mouse mode (for games/3D apps).
  // This is a client-side feature that affects only this client's input behavior.
  // Multiple clients can independently enable/disable relative mouse mode without
  // affecting each other. The server simply processes mouse events as they arrive,
  // whether absolute (MOUSE_TYPE_MOVE) or relative (MOUSE_TYPE_MOVE_RELATIVE).
  // Note: This feature is only available in Flutter client. Sciter client does not support this.
  final relativeMouseMode = false.obs;
  final RelativeMouseAccumulator _relativeMouseAccumulator =
      RelativeMouseAccumulator();
  // Pointer lock center in LOCAL widget coordinates (for delta calculation)
  Offset? _pointerLockCenterLocal;
  // Pointer lock center in SCREEN coordinates (for OS cursor re-centering)
  Offset? _pointerLockCenterScreen;
  // Pointer region top-left in Flutter view coordinates.
  // Computed from PointerEvent.position - PointerEvent.localPosition.
  Offset? _pointerRegionTopLeftGlobal;
  // Last pointer position in LOCAL widget coordinates (fallback when center is not ready).
  Offset? _lastPointerLocalPos;
  // Track how many times we've hidden the OS cursor (for proper reference count management on Windows)
  // Windows ShowCursor uses a reference counting mechanism - we need to match hide/show calls exactly.
  int _cursorHideCount = 0;
  // Size of the remote image widget (for center calculation)
  Size? _imageWidgetSize;
  // Debounce timer for relative mouse mode toggle to prevent race conditions
  // between Rust rdev grab loop and Flutter keyboard handling
  DateTime? _lastRelativeMouseToggle;
  // Callback to cancel external throttle timer when relative mouse mode is disabled
  VoidCallback? onRelativeMouseModeDisabled;

  bool _queryOtherWindowCoords = false;
  Rect? _windowRect;
  List<RemoteWindowCoords> _remoteWindowCoords = [];

  late final SessionID sessionId;

  bool get keyboardPerm => parent.target!.ffiModel.keyboard;
  String get id => parent.target?.id ?? '';
  String? get peerPlatform => parent.target?.ffiModel.pi.platform;
  String get peerVersion => parent.target?.ffiModel.pi.version ?? '';
  bool get isViewOnly => parent.target!.ffiModel.viewOnly;
  bool get showMyCursor => parent.target!.ffiModel.showMyCursor;
  double get devicePixelRatio => parent.target!.canvasModel.devicePixelRatio;
  bool get isViewCamera => parent.target!.connType == ConnType.viewCamera;
  int get trackpadSpeed => _trackpadSpeed;
  bool get useEdgeScroll => parent.target!.canvasModel.scrollStyle == ScrollStyle.scrolledge;

  /// Check if the connected server supports relative mouse mode.
  /// Returns true if server version >= kMinVersionForRelativeMouseMode (from consts.dart).
  bool get isRelativeMouseModeSupported {
    if (peerVersion.isEmpty) return false;
    return versionCmp(peerVersion, kMinVersionForRelativeMouseMode) >= 0;
  }

  InputModel(this.parent) {
    sessionId = parent.target!.sessionId;
  }

  // This function must be called after the peer info is received.
  // Because `sessionGetKeyboardMode` relies on the peer version.
  updateKeyboardMode() async {
    // * Currently mobile does not enable map mode
    if (isDesktop || isWebDesktop) {
      keyboardMode = await bind.sessionGetKeyboardMode(sessionId: sessionId) ??
          kKeyLegacyMode;
    }
  }

  /// Updates the trackpad speed based on the session value.
  ///
  /// The expected format of the retrieved value is a string that can be parsed into a double.
  /// If parsing fails or the value is out of bounds (less than `kMinTrackpadSpeed` or greater
  /// than `kMaxTrackpadSpeed`), the trackpad speed is reset to the default
  /// value (`kDefaultTrackpadSpeed`).
  ///
  /// Bounds:
  /// - Minimum: `kMinTrackpadSpeed`
  /// - Maximum: `kMaxTrackpadSpeed`
  /// - Default: `kDefaultTrackpadSpeed`
  Future<void> updateTrackpadSpeed() async {
    _trackpadSpeed =
        (await bind.sessionGetTrackpadSpeed(sessionId: sessionId) ??
            kDefaultTrackpadSpeed);
    if (_trackpadSpeed < kMinTrackpadSpeed ||
        _trackpadSpeed > kMaxTrackpadSpeed) {
      _trackpadSpeed = kDefaultTrackpadSpeed;
    }
    _trackpadSpeedInner = _trackpadSpeed / 100.0;
  }

  void handleKeyDownEventModifiers(KeyEvent e) {
    KeyUpEvent upEvent(e) => KeyUpEvent(
          physicalKey: e.physicalKey,
          logicalKey: e.logicalKey,
          timeStamp: e.timeStamp,
        );
    if (e.logicalKey == LogicalKeyboardKey.altLeft) {
      if (!alt) {
        alt = true;
      }
      toReleaseKeys.lastLAltKeyEvent = upEvent(e);
    } else if (e.logicalKey == LogicalKeyboardKey.altRight) {
      if (!alt) {
        alt = true;
      }
      toReleaseKeys.lastLAltKeyEvent = upEvent(e);
    } else if (e.logicalKey == LogicalKeyboardKey.controlLeft) {
      if (!ctrl) {
        ctrl = true;
      }
      toReleaseKeys.lastLCtrlKeyEvent = upEvent(e);
    } else if (e.logicalKey == LogicalKeyboardKey.controlRight) {
      if (!ctrl) {
        ctrl = true;
      }
      toReleaseKeys.lastRCtrlKeyEvent = upEvent(e);
    } else if (e.logicalKey == LogicalKeyboardKey.shiftLeft) {
      if (!shift) {
        shift = true;
      }
      toReleaseKeys.lastLShiftKeyEvent = upEvent(e);
    } else if (e.logicalKey == LogicalKeyboardKey.shiftRight) {
      if (!shift) {
        shift = true;
      }
      toReleaseKeys.lastRShiftKeyEvent = upEvent(e);
    } else if (e.logicalKey == LogicalKeyboardKey.metaLeft) {
      if (!command) {
        command = true;
      }
      toReleaseKeys.lastLCommandKeyEvent = upEvent(e);
    } else if (e.logicalKey == LogicalKeyboardKey.metaRight) {
      if (!command) {
        command = true;
      }
      toReleaseKeys.lastRCommandKeyEvent = upEvent(e);
    } else if (e.logicalKey == LogicalKeyboardKey.superKey) {
      if (!command) {
        command = true;
      }
      toReleaseKeys.lastSuperKeyEvent = upEvent(e);
    }
  }

  void handleKeyUpEventModifiers(KeyEvent e) {
    if (e.logicalKey == LogicalKeyboardKey.altLeft) {
      alt = false;
      toReleaseKeys.lastLAltKeyEvent = null;
    } else if (e.logicalKey == LogicalKeyboardKey.altRight) {
      alt = false;
      toReleaseKeys.lastRAltKeyEvent = null;
    } else if (e.logicalKey == LogicalKeyboardKey.controlLeft) {
      ctrl = false;
      toReleaseKeys.lastLCtrlKeyEvent = null;
    } else if (e.logicalKey == LogicalKeyboardKey.controlRight) {
      ctrl = false;
      toReleaseKeys.lastRCtrlKeyEvent = null;
    } else if (e.logicalKey == LogicalKeyboardKey.shiftLeft) {
      shift = false;
      toReleaseKeys.lastLShiftKeyEvent = null;
    } else if (e.logicalKey == LogicalKeyboardKey.shiftRight) {
      shift = false;
      toReleaseKeys.lastRShiftKeyEvent = null;
    } else if (e.logicalKey == LogicalKeyboardKey.metaLeft) {
      command = false;
      toReleaseKeys.lastLCommandKeyEvent = null;
    } else if (e.logicalKey == LogicalKeyboardKey.metaRight) {
      command = false;
      toReleaseKeys.lastRCommandKeyEvent = null;
    } else if (e.logicalKey == LogicalKeyboardKey.superKey) {
      command = false;
      toReleaseKeys.lastSuperKeyEvent = null;
    }
  }

  KeyEventResult handleRawKeyEvent(RawKeyEvent e) {
    if (isViewOnly) return KeyEventResult.handled;
    if (isViewCamera) return KeyEventResult.handled;
    if (!isInputSourceFlutter) {
      if (isDesktop) {
        return KeyEventResult.handled;
      } else if (isWeb) {
        return KeyEventResult.ignored;
      }
    }

    final key = e.logicalKey;
    if (e is RawKeyDownEvent) {
      if (!e.repeat) {
        if (e.isAltPressed && !alt) {
          alt = true;
        } else if (e.isControlPressed && !ctrl) {
          ctrl = true;
        } else if (e.isShiftPressed && !shift) {
          shift = true;
        } else if (e.isMetaPressed && !command) {
          command = true;
        }
      }
      toReleaseRawKeys.updateKeyDown(key, e);
    }
    if (e is RawKeyUpEvent) {
      if (key == LogicalKeyboardKey.altLeft ||
          key == LogicalKeyboardKey.altRight) {
        alt = false;
      } else if (key == LogicalKeyboardKey.controlLeft ||
          key == LogicalKeyboardKey.controlRight) {
        ctrl = false;
      } else if (key == LogicalKeyboardKey.shiftRight ||
          key == LogicalKeyboardKey.shiftLeft) {
        shift = false;
      } else if (key == LogicalKeyboardKey.metaLeft ||
          key == LogicalKeyboardKey.metaRight ||
          key == LogicalKeyboardKey.superKey) {
        command = false;
      }

      toReleaseRawKeys.updateKeyUp(key, e);
    }

    // * Currently mobile does not enable map mode
    if ((isDesktop || isWebDesktop) && keyboardMode == kKeyMapMode) {
      mapKeyboardModeRaw(e);
    } else {
      legacyKeyboardModeRaw(e);
    }

    return KeyEventResult.handled;
  }

  KeyEventResult handleKeyEvent(KeyEvent e) {
    if (isViewOnly) return KeyEventResult.handled;
    if (isViewCamera) return KeyEventResult.handled;
    if (!isInputSourceFlutter) {
      if (isDesktop) {
        return KeyEventResult.handled;
      } else if (isWeb) {
        return KeyEventResult.ignored;
      }
    }
    if (isWindows || isLinux) {
      // Ignore meta keys. Because flutter window will loose focus if meta key is pressed.
      if (e.physicalKey == PhysicalKeyboardKey.metaLeft ||
          e.physicalKey == PhysicalKeyboardKey.metaRight) {
        return KeyEventResult.handled;
      }
    }

    // Shortcut: Ctrl+Shift+G (Windows/Linux) or Cmd+Shift+G (macOS) toggles relative mouse mode.
    // This check handles the shortcut when rdev grab is not active (e.g., Flutter-only keyboard handling).
    // There is also a duplicate check in Rust's keyboard.rs for the rdev grab loop to intercept physical
    // keyboard events. Both checks are necessary to ensure the shortcut works in all scenarios.
    if (e is KeyDownEvent && e.logicalKey == LogicalKeyboardKey.keyG) {
      if (isDesktop && keyboardPerm && !isViewCamera) {
        final isModifierPressed = isMacOS ? command : ctrl;
        if (isModifierPressed && shift) {
          toggleRelativeMouseMode();
          return KeyEventResult.handled;
        }
      }
    }

    if (e is KeyUpEvent) {
      handleKeyUpEventModifiers(e);
    } else if (e is KeyDownEvent) {
      handleKeyDownEventModifiers(e);
    }

    bool isMobileAndMapMode = false;
    if (isMobile) {
      // Do not use map mode if mobile -> Android. Android does not support map mode for now.
      // Because simulating the physical key events(uhid) which requires root permission is not supported.
      if (peerPlatform != kPeerPlatformAndroid) {
        if (isIOS) {
          isMobileAndMapMode = true;
        } else {
          // The physicalKey.usbHidUsage may be not correct for soft keyboard on Android.
          // iOS does not have this issue.
          // 1. Open the soft keyboard on Android
          // 2. Switch to input method like zh/ko/ja
          // 3. Click Backspace and Enter on the soft keyboard or physical keyboard
          // 4. The physicalKey.usbHidUsage is not correct.
          // PhysicalKeyboardKey#8ac83(usbHidUsage: "0x1100000042", debugName: "Key with ID 0x1100000042")
          // LogicalKeyboardKey#2604c(keyId: "0x10000000d", keyLabel: "Enter", debugName: "Enter")
          //
          // The correct PhysicalKeyboardKey should be
          // PhysicalKeyboardKey#e14a9(usbHidUsage: "0x00070028", debugName: "Enter")
          // https://github.com/flutter/flutter/issues/157771
          // We cannot use the debugName to determine the key is correct or not, because it's null in release mode.
          // The normal `usbHidUsage` for keyboard shoud be between [0x00000010, 0x000c029f]
          // https://github.com/flutter/flutter/blob/c051b69e2a2224300e20d93dbd15f4b91e8844d1/packages/flutter/lib/src/services/keyboard_key.g.dart#L5332 - 5600
          final isNormalHsbHidUsage = (e.physicalKey.usbHidUsage >> 20) == 0;
          isMobileAndMapMode = isNormalHsbHidUsage &&
              // No need to check `!['Backspace', 'Enter'].contains(e.logicalKey.keyLabel)`
              // But we still add it for more reliability.
              !['Backspace', 'Enter'].contains(e.logicalKey.keyLabel);
        }
      }
    }
    final isDesktopAndMapMode =
        isDesktop || (isWebDesktop && keyboardMode == kKeyMapMode);
    if (isMobileAndMapMode || isDesktopAndMapMode) {
      // FIXME: e.character is wrong for dead keys, eg: ^ in de
      newKeyboardMode(
          e.character ?? '',
          e.physicalKey.usbHidUsage & 0xFFFF,
          // Show repeat event be converted to "release+press" events?
          e is KeyDownEvent || e is KeyRepeatEvent);
    } else {
      legacyKeyboardMode(e);
    }

    return KeyEventResult.handled;
  }

  /// Send Key Event
  void newKeyboardMode(String character, int usbHid, bool down) {
    const capslock = 1;
    const numlock = 2;
    const scrolllock = 3;
    int lockModes = 0;
    if (HardwareKeyboard.instance.lockModesEnabled
        .contains(KeyboardLockMode.capsLock)) {
      lockModes |= (1 << capslock);
    }
    if (HardwareKeyboard.instance.lockModesEnabled
        .contains(KeyboardLockMode.numLock)) {
      lockModes |= (1 << numlock);
    }
    if (HardwareKeyboard.instance.lockModesEnabled
        .contains(KeyboardLockMode.scrollLock)) {
      lockModes |= (1 << scrolllock);
    }
    bind.sessionHandleFlutterKeyEvent(
        sessionId: sessionId,
        character: character,
        usbHid: usbHid,
        lockModes: lockModes,
        downOrUp: down);
  }

  void mapKeyboardModeRaw(RawKeyEvent e) {
    int positionCode = -1;
    int platformCode = -1;
    bool down;

    if (e.data is RawKeyEventDataMacOs) {
      RawKeyEventDataMacOs newData = e.data as RawKeyEventDataMacOs;
      positionCode = newData.keyCode;
      platformCode = newData.keyCode;
    } else if (e.data is RawKeyEventDataWindows) {
      RawKeyEventDataWindows newData = e.data as RawKeyEventDataWindows;
      positionCode = newData.scanCode;
      platformCode = newData.keyCode;
    } else if (e.data is RawKeyEventDataLinux) {
      RawKeyEventDataLinux newData = e.data as RawKeyEventDataLinux;
      // scanCode and keyCode of RawKeyEventDataLinux are incorrect.
      // 1. scanCode means keycode
      // 2. keyCode means keysym
      positionCode = newData.scanCode;
      platformCode = newData.keyCode;
    } else if (e.data is RawKeyEventDataAndroid) {
      RawKeyEventDataAndroid newData = e.data as RawKeyEventDataAndroid;
      positionCode = newData.scanCode + 8;
      platformCode = newData.keyCode;
    } else {}

    if (e is RawKeyDownEvent) {
      down = true;
    } else {
      down = false;
    }
    inputRawKey(e.character ?? '', platformCode, positionCode, down);
  }

  /// Send raw Key Event
  void inputRawKey(String name, int platformCode, int positionCode, bool down) {
    const capslock = 1;
    const numlock = 2;
    const scrolllock = 3;
    int lockModes = 0;
    if (HardwareKeyboard.instance.lockModesEnabled
        .contains(KeyboardLockMode.capsLock)) {
      lockModes |= (1 << capslock);
    }
    if (HardwareKeyboard.instance.lockModesEnabled
        .contains(KeyboardLockMode.numLock)) {
      lockModes |= (1 << numlock);
    }
    if (HardwareKeyboard.instance.lockModesEnabled
        .contains(KeyboardLockMode.scrollLock)) {
      lockModes |= (1 << scrolllock);
    }
    bind.sessionHandleFlutterRawKeyEvent(
        sessionId: sessionId,
        name: name,
        platformCode: platformCode,
        positionCode: positionCode,
        lockModes: lockModes,
        downOrUp: down);
  }

  void legacyKeyboardModeRaw(RawKeyEvent e) {
    if (e is RawKeyDownEvent) {
      if (e.repeat) {
        sendRawKey(e, press: true);
      } else {
        sendRawKey(e, down: true);
      }
    }
    if (e is RawKeyUpEvent) {
      sendRawKey(e);
    }
  }

  void sendRawKey(RawKeyEvent e, {bool? down, bool? press}) {
    // for maximum compatibility
    final label = physicalKeyMap[e.physicalKey.usbHidUsage] ??
        logicalKeyMap[e.logicalKey.keyId] ??
        e.logicalKey.keyLabel;
    inputKey(label, down: down, press: press ?? false);
  }

  void legacyKeyboardMode(KeyEvent e) {
    if (e is KeyDownEvent) {
      sendKey(e, down: true);
    } else if (e is KeyRepeatEvent) {
      sendKey(e, press: true);
    } else if (e is KeyUpEvent) {
      sendKey(e);
    }
  }

  void sendKey(KeyEvent e, {bool? down, bool? press}) {
    // for maximum compatibility
    final label = physicalKeyMap[e.physicalKey.usbHidUsage] ??
        logicalKeyMap[e.logicalKey.keyId] ??
        e.logicalKey.keyLabel;
    inputKey(label, down: down, press: press ?? false);
  }

  /// Send key stroke event.
  /// [down] indicates the key's state(down or up).
  /// [press] indicates a click event(down and up).
  void inputKey(String name, {bool? down, bool? press}) {
    if (!keyboardPerm) return;
    if (isViewCamera) return;
    bind.sessionInputKey(
        sessionId: sessionId,
        name: name,
        down: down ?? false,
        press: press ?? true,
        alt: alt,
        ctrl: ctrl,
        shift: shift,
        command: command);
  }

  static Map<String, dynamic> getMouseEventMove() => {
        'type': _kMouseEventMove,
        'buttons': 0,
      };

  Map<String, dynamic> _getMouseEvent(PointerEvent evt, String type) {
    final Map<String, dynamic> out = {};

    // Check update event type and set buttons to be sent.
    int buttons = _lastButtons;
    if (type == _kMouseEventMove) {
      // flutter may emit move event if one button is pressed and another button
      // is pressing or releasing.
      if (evt.buttons != _lastButtons) {
        // For simplicity
        // Just consider 3 - 1 ((Left + Right buttons) - Left button)
        // Do not consider 2 - 1 (Right button - Left button)
        // or 6 - 5 ((Right + Mid buttons) - (Left + Mid buttons))
        // and so on
        buttons = evt.buttons - _lastButtons;
        if (buttons > 0) {
          type = _kMouseEventDown;
        } else {
          type = _kMouseEventUp;
          buttons = -buttons;
        }
      }
    } else {
      if (evt.buttons != 0) {
        buttons = evt.buttons;
      }
    }
    _lastButtons = evt.buttons;

    out['buttons'] = buttons;
    out['type'] = type;
    return out;
  }

  /// Send a mouse tap event(down and up).
  Future<void> tap(MouseButtons button) async {
    await sendMouse('down', button);
    await sendMouse('up', button);
  }

  Future<void> tapDown(MouseButtons button) async {
    await sendMouse('down', button);
  }

  Future<void> tapUp(MouseButtons button) async {
    await sendMouse('up', button);
  }

  /// Send scroll event with scroll distance [y].
  Future<void> scroll(int y) async {
    if (isViewCamera) return;
    await bind.sessionSendMouse(
        sessionId: sessionId,
        msg: json
            .encode(modify({'id': id, 'type': 'wheel', 'y': y.toString()})));
  }

  /// Reset key modifiers to false, including [shift], [ctrl], [alt] and [command].
  void resetModifiers() {
    shift = ctrl = alt = command = false;
  }

  /// Modify the given modifier map [evt] based on current modifier key status.
  Map<String, dynamic> modify(Map<String, dynamic> evt) {
    if (ctrl) evt['ctrl'] = 'true';
    if (shift) evt['shift'] = 'true';
    if (alt) evt['alt'] = 'true';
    if (command) evt['command'] = 'true';
    return evt;
  }

  /// Send mouse press event.
  Future<void> sendMouse(String type, MouseButtons button) async {
    if (!keyboardPerm) return;
    if (isViewCamera) return;
    await bind.sessionSendMouse(
        sessionId: sessionId,
        msg: json.encode(modify({'type': type, 'buttons': button.value})));
  }

  void enterOrLeave(bool enter) {
    toReleaseKeys.release(handleKeyEvent);
    toReleaseRawKeys.release(handleRawKeyEvent);
    _pointerMovedAfterEnter = false;
    _pointerInsideImage = enter;

    // Fix status
    if (!enter) {
      resetModifiers();
      // Ensure cursor is shown and unclipped when leaving the image area.
      if (relativeMouseMode.value) {
        _showOsCursor();
        _releaseCursorClip();
      }
    } else {
      // Re-hide cursor and re-establish pointer lock when (re-)entering.
      if (relativeMouseMode.value) {
        _hideOsCursor();
        updatePointerLockCenter().then((_) => _recenterMouse());
      }
    }
    _flingTimer?.cancel();
    if (!isInputSourceFlutter) {
      bind.sessionEnterOrLeave(sessionId: sessionId, enter: enter);
    }
    if (!isWeb && enter) {
      bind.setCurSessionId(sessionId: sessionId);
    }
  }

  /// Send mouse movement event with distance in [x] and [y].
  Future<void> moveMouse(double x, double y) async {
    if (!keyboardPerm) return;
    if (isViewCamera) return;
    var x2 = x.toInt();
    var y2 = y.toInt();
    await bind.sessionSendMouse(
        sessionId: sessionId,
        msg: json.encode(modify({'x': '$x2', 'y': '$y2'})));
  }

  /// Send relative mouse movement event with delta [dx] and [dy].
  /// Uses fractional accumulator to handle sub-pixel movements.
  /// Delta values are clamped to [-kMaxRelativeMouseDelta, kMaxRelativeMouseDelta]
  /// to prevent overflow on server side.
  Future<void> sendRelativeMouseMove(double dx, double dy) async {
    if (!keyboardPerm) return;
    if (isViewCamera) return;
    // Only available on desktop platforms
    if (!isDesktop) return;

    final delta = _relativeMouseAccumulator.add(dx, dy,
        maxDelta: kMaxRelativeMouseDelta);
    if (delta == null) return;

    await bind.sessionSendMouse(
        sessionId: sessionId,
        msg: json.encode(modify(
            {'type': 'move_relative', 'x': '${delta.x}', 'y': '${delta.y}'})));

    // Re-center mouse after movement to prevent hitting screen edges
    _recenterMouse();
  }

  /// Re-center the mouse cursor to the stored center position.
  /// This is used in relative mouse mode to prevent the cursor from hitting screen edges.
  void _recenterMouse() {
    if (!relativeMouseMode.value) return;
    if (!_pointerInsideImage) return;
    if (_pointerLockCenterScreen != null) {
      bind.mainSetCursorPosition(
          x: _pointerLockCenterScreen!.dx.toInt(),
          y: _pointerLockCenterScreen!.dy.toInt());
    }
  }

  /// Update the pointer lock center position based on current window frame.
  /// Should be called when entering relative mouse mode or when window moves.
  /// [localCenter] is the center in local widget coordinates (from the remote image widget).
  ///
  /// Note: This method requires _imageWidgetSize to be set (via updateImageWidgetSize)
  /// before relative mouse mode can be properly enabled. The setRelativeMouseMode()
  /// method enforces this requirement.
  Future<void> updatePointerLockCenter({Offset? localCenter}) async {
    if (!isDesktop) return;

    // Null safety check for kWindowId
    if (kWindowId == null) {
      debugPrint('updatePointerLockCenter: kWindowId is null, cannot update pointer lock center');
      return;
    }

    try {
      final wc = WindowController.fromWindowId(kWindowId!);
      final frame = await wc.getFrame();

      // Validate window frame dimensions to prevent invalid center calculation
      if (frame.width <= 0 || frame.height <= 0) {
        debugPrint('updatePointerLockCenter: invalid window frame dimensions (${frame.width}x${frame.height})');
        if (relativeMouseMode.value) {
          debugPrint('Disabling relative mouse mode due to invalid window frame');
          _disableRelativeMouseModeWithCleanup();
        }
        return;
      }

      // Store local center for delta calculation
      if (localCenter != null) {
        _pointerLockCenterLocal = localCenter;
      } else if (_imageWidgetSize != null) {
        // Use the stored image widget size (always available when relative mode is active)
        _pointerLockCenterLocal = Offset(
          _imageWidgetSize!.width / 2,
          _imageWidgetSize!.height / 2,
        );
      } else {
        // This should not happen if setRelativeMouseMode() is called properly,
        // but handle gracefully by disabling relative mouse mode
        debugPrint('updatePointerLockCenter: _imageWidgetSize is null, disabling relative mouse mode');
        if (relativeMouseMode.value) {
          _disableRelativeMouseModeWithCleanup();
        }
        return;
      }

      // Calculate screen coordinates for OS cursor positioning.
      // Try to map the local widget center to OS screen coordinates.
      // If we don't have enough information (e.g., no pointer events yet),
      // fall back to the window frame center.
      final scale = ui.window.devicePixelRatio;
      if (_pointerRegionTopLeftGlobal != null && scale > 0) {
        // Estimate the client-area top-left in screen coordinates.
        // On Windows, getFrame() matches GetWindowRect (outer frame), while Flutter events are in
        // client-area coordinates. We approximate non-client offsets based on the difference between
        // window frame size and Flutter view physical size.
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

      // On Windows, when relative mouse mode is active, also clip the OS cursor
      // to the window rect to better emulate pointer lock.
      if (relativeMouseMode.value && isWindows) {
        _applyCursorClipForFrame(frame);
      }
    } catch (e) {
      debugPrint('Failed to get window frame for pointer lock center: $e');
      // If we can't get the window frame and relative mouse mode is active,
      // disable it to prevent user getting stuck with hidden cursor.
      if (relativeMouseMode.value) {
        debugPrint('Disabling relative mouse mode due to window frame error');
        _disableRelativeMouseModeWithCleanup();
      } else {
        // If not in relative mode, just clear the local state
        _pointerLockCenterLocal = null;
        _pointerLockCenterScreen = null;
      }
    }
  }

  /// Reset relative mouse mode state variables.
  /// This is the common cleanup logic used by multiple methods.
  void _resetRelativeMouseModeState() {
    _relativeMouseAccumulator.reset();
    _pointerLockCenterLocal = null;
    _pointerLockCenterScreen = null;
    _pointerRegionTopLeftGlobal = null;
    _lastPointerLocalPos = null;
    _pointerInsideImage = false;
  }

  /// Internal helper to disable relative mouse mode with guaranteed cleanup.
  /// This method ensures cursor is restored even if normal cleanup fails.
  void _disableRelativeMouseModeWithCleanup() {
    // Force restore cursor first to guarantee visibility
    _forceRestoreCursor();
    _releaseCursorClip();
    // Then set the state
    relativeMouseMode.value = false;
    _resetRelativeMouseModeState();
    // Notify listener to cancel any pending throttle timers
    onRelativeMouseModeDisabled?.call();
  }

  /// Apply OS-level cursor clipping to the given window frame (Windows only).
  /// Callers must ensure this is only called on Windows.
  void _applyCursorClipForFrame(Rect frame) {
    final left = frame.left.toInt();
    final top = frame.top.toInt();
    final right = (frame.left + frame.width).toInt();
    final bottom = (frame.top + frame.height).toInt();
    bind.mainClipCursor(
      left: left,
      top: top,
      right: right,
      bottom: bottom,
      enable: true,
    );
  }

  /// Release any previously applied OS-level cursor clipping (Windows only).
  void _releaseCursorClip() {
    if (!isWindows) return;
    bind.mainClipCursor(
      left: 0,
      top: 0,
      right: 0,
      bottom: 0,
      enable: false,
    );
  }

  /// Get the current image widget size (for comparison to avoid unnecessary updates).
  Size? get imageWidgetSize => _imageWidgetSize;

  /// Update the image widget size for center calculation.
  /// Should be called from the remote page when the widget size changes.
  void updateImageWidgetSize(Size size) {
    _imageWidgetSize = size;
    // If in relative mouse mode, update the center
    if (relativeMouseMode.value) {
      _pointerLockCenterLocal = Offset(size.width / 2, size.height / 2);
    }
  }

  /// Toggle relative mouse mode on/off.
  /// Includes debounce protection to prevent race conditions when both
  /// Rust rdev grab loop and Flutter keyboard handling detect the shortcut.
  void toggleRelativeMouseMode() {
    // Debounce: ignore toggles within kRelativeMouseModeToggleDebounceMs
    // to prevent double-toggle from race condition
    final now = DateTime.now();
    if (_lastRelativeMouseToggle != null &&
        now.difference(_lastRelativeMouseToggle!).inMilliseconds < kRelativeMouseModeToggleDebounceMs) {
      debugPrint('Ignoring relative mouse mode toggle: debounce protection');
      return;
    }
    _lastRelativeMouseToggle = now;
    setRelativeMouseMode(!relativeMouseMode.value);
  }

  /// Set relative mouse mode.
  /// Returns false if the server doesn't support relative mouse mode or if
  /// the image widget size is not yet available (required for proper center calculation).
  bool setRelativeMouseMode(bool enabled) {
    if (!isDesktop) return false;
    // Check server version support before enabling
    if (enabled && !isRelativeMouseModeSupported) {
      debugPrint('Relative mouse mode not supported by server version: $peerVersion (requires >= $kMinVersionForRelativeMouseMode)');
      return false;
    }
    // Ensure image widget size is available for proper center calculation
    if (enabled && _imageWidgetSize == null) {
      debugPrint('Relative mouse mode cannot be enabled: image widget size not yet available');
      return false;
    }

    if (enabled) {
      // Entering relative mode - hide cursor, update center and clip cursor.
      // Wrap in try-catch to guarantee cleanup on any failure.
      try {
        relativeMouseMode.value = true;
        _hideOsCursor();
        updatePointerLockCenter().then((_) => _recenterMouse()).catchError((e) {
          debugPrint('Failed to update pointer lock center: $e');
          // Cleanup on async failure
          _disableRelativeMouseModeWithCleanup();
        });
      } catch (e) {
        debugPrint('Failed to enable relative mouse mode: $e');
        // Ensure cleanup on any synchronous failure
        _disableRelativeMouseModeWithCleanup();
        return false;
      }
    } else {
      // Exiting relative mode - show cursor, release clip and reset state.
      _showOsCursor();
      _releaseCursorClip();
      relativeMouseMode.value = false;
      _resetRelativeMouseModeState();
      // Notify listener to cancel any pending throttle timers
      onRelativeMouseModeDisabled?.call();
    }
    // Note: relative mouse mode is not persisted to config
    return true;
  }

  /// Hide OS cursor (with reference count protection)
  /// Windows ShowCursor uses reference counting - each hide decrements, each show increments.
  /// We track our own count to ensure we only show as many times as we've hidden.
  void _hideOsCursor() {
    if (_cursorHideCount == 0) {
      bind.mainShowCursor(show: false);
      _cursorHideCount = 1;
    }
  }

  /// Show OS cursor (with reference count protection)
  /// Only shows if we previously hid the cursor, preventing reference count imbalance.
  void _showOsCursor() {
    if (_cursorHideCount > 0) {
      bind.mainShowCursor(show: true);
      _cursorHideCount = 0;
    }
  }

  /// Force restore cursor visibility - used during cleanup to ensure cursor is visible.
  /// This handles edge cases like crashes or unexpected state by resetting to a known good state.
  void _forceRestoreCursor() {
    while (_cursorHideCount > 0) {
      bind.mainShowCursor(show: true);
      _cursorHideCount--;
    }
  }

  /// Dispose resources related to relative mouse mode.
  /// Called when the session is closed to ensure proper cleanup.
  void disposeRelativeMouseMode() {
    // Force restore cursor in case of any state inconsistency
    _forceRestoreCursor();
    _releaseCursorClip();
    _resetRelativeMouseModeState();
    _imageWidgetSize = null;
    _lastRelativeMouseToggle = null;
    // Clear callback to avoid memory leaks and stale references
    onRelativeMouseModeDisabled = null;
    relativeMouseMode.value = false;
  }

  /// Called when the window loses focus.
  /// Temporarily releases cursor constraints to allow user to interact with other apps.
  void onWindowBlur() {
    if (!relativeMouseMode.value) return;
    _showOsCursor();
    _releaseCursorClip();
  }

  /// Called when the window regains focus.
  /// Restores cursor constraints for relative mouse mode.
  void onWindowFocus() {
    if (!relativeMouseMode.value) return;
    // Guard: image widget size must be available for proper center calculation
    if (_imageWidgetSize == null) {
      debugPrint('onWindowFocus: _imageWidgetSize is null, disabling relative mouse mode');
      _disableRelativeMouseModeWithCleanup();
      return;
    }
    _hideOsCursor();
    updatePointerLockCenter().then((_) => _recenterMouse());
  }

  /// Handle relative mouse movement based on current position.
  /// This is a common method used by both onPointHoverImage and onPointMoveImage.
  /// Returns true if the event was handled in relative mode, false otherwise.
  bool _handleRelativeMouseMove(Offset localPosition) {
    if (!relativeMouseMode.value) return false;

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

  void onPointHoverImage(PointerHoverEvent e) {
    _stopFling = true;
    if (isViewOnly && !showMyCursor) return;
    if (e.kind != ui.PointerDeviceKind.mouse) return;

    // Keep the pointer region origin in sync for mapping local -> global.
    _pointerRegionTopLeftGlobal = e.position - e.localPosition;

    if (!isPhysicalMouse.value) {
      isPhysicalMouse.value = true;
    }
    if (isPhysicalMouse.value) {
      if (!_handleRelativeMouseMove(e.localPosition)) {
        handleMouse(_getMouseEvent(e, _kMouseEventMove), e.position,
            edgeScroll: useEdgeScroll);
      }
    }
  }

  void onPointerPanZoomStart(PointerPanZoomStartEvent e) {
    _lastScale = 1.0;
    _stopFling = true;
    if (isViewOnly) return;
    if (isViewCamera) return;
    if (peerPlatform == kPeerPlatformAndroid) {
      handlePointerEvent('touch', kMouseEventTypePanStart, e.position);
    }
  }

  // https://docs.flutter.dev/release/breaking-changes/trackpad-gestures
  void onPointerPanZoomUpdate(PointerPanZoomUpdateEvent e) {
    if (isViewOnly) return;
    if (isViewCamera) return;
    if (peerPlatform != kPeerPlatformAndroid) {
      final scale = ((e.scale - _lastScale) * 1000).toInt();
      _lastScale = e.scale;

      if (scale != 0) {
        bind.sessionSendPointer(
            sessionId: sessionId,
            msg: json.encode(
                PointerEventToRust(kPointerEventKindTouch, 'scale', scale)
                    .toJson()));
        return;
      }
    }

    var delta = e.panDelta * _trackpadSpeedInner;
    if (isMacOS && peerPlatform == kPeerPlatformWindows) {
      delta *= _trackpadAdjustMacToWin;
    }
    _trackpadLastDelta = delta;

    var x = delta.dx.toInt();
    var y = delta.dy.toInt();
    if (peerPlatform == kPeerPlatformLinux) {
      _trackpadScrollUnsent += (delta * _trackpadAdjustPeerLinux);
      x = _trackpadScrollUnsent.dx.truncate();
      y = _trackpadScrollUnsent.dy.truncate();
      _trackpadScrollUnsent -= Offset(x.toDouble(), y.toDouble());
    } else {
      if (x == 0 && y == 0) {
        final thr = 0.1;
        if (delta.dx.abs() > delta.dy.abs()) {
          x = delta.dx > thr ? 1 : (delta.dx < -thr ? -1 : 0);
        } else {
          y = delta.dy > thr ? 1 : (delta.dy < -thr ? -1 : 0);
        }
      }
    }
    if (x != 0 || y != 0) {
      if (peerPlatform == kPeerPlatformAndroid) {
        handlePointerEvent('touch', kMouseEventTypePanUpdate,
            Offset(x.toDouble(), y.toDouble()));
      } else {
        if (isViewCamera) return;
        bind.sessionSendMouse(
            sessionId: sessionId,
            msg: '{"type": "trackpad", "x": "$x", "y": "$y"}');
      }
    }
  }

  void _scheduleFling(double x, double y, int delay) {
    if (isViewCamera) return;
    if ((x == 0 && y == 0) || _stopFling) {
      _fling = false;
      return;
    }

    _flingTimer = Timer(Duration(milliseconds: delay), () {
      if (_stopFling) {
        _fling = false;
        return;
      }

      final d = 0.97;
      x *= d;
      y *= d;

      // Try set delta (x,y) and delay.
      var dx = x.toInt();
      var dy = y.toInt();
      if (parent.target?.ffiModel.pi.platform == kPeerPlatformLinux) {
        dx = (x * _trackpadAdjustPeerLinux).toInt();
        dy = (y * _trackpadAdjustPeerLinux).toInt();
      }

      var delay = _flingBaseDelay;

      if (dx == 0 && dy == 0) {
        _fling = false;
        return;
      }

      bind.sessionSendMouse(
          sessionId: sessionId,
          msg: '{"type": "trackpad", "x": "$dx", "y": "$dy"}');
      _scheduleFling(x, y, delay);
    });
  }

  void waitLastFlingDone() {
    if (_fling) {
      _stopFling = true;
    }
    for (var i = 0; i < 5; i++) {
      if (!_fling) {
        break;
      }
      sleep(Duration(milliseconds: 10));
    }
    _flingTimer?.cancel();
  }

  void onPointerPanZoomEnd(PointerPanZoomEndEvent e) {
    if (isViewCamera) return;
    if (peerPlatform == kPeerPlatformAndroid) {
      handlePointerEvent('touch', kMouseEventTypePanEnd, e.position);
      return;
    }

    bind.sessionSendPointer(
        sessionId: sessionId,
        msg: json.encode(
            PointerEventToRust(kPointerEventKindTouch, 'scale', 0).toJson()));

    waitLastFlingDone();
    _stopFling = false;

    // 2.0 is an experience value
    double minFlingValue = 2.0 * _trackpadSpeedInner;
    if (isMacOS && peerPlatform == kPeerPlatformWindows) {
      minFlingValue *= _trackpadAdjustMacToWin;
    }
    if (_trackpadLastDelta.dx.abs() > minFlingValue ||
        _trackpadLastDelta.dy.abs() > minFlingValue) {
      _fling = true;
      _scheduleFling(
          _trackpadLastDelta.dx, _trackpadLastDelta.dy, _flingBaseDelay);
    }
    _trackpadLastDelta = Offset.zero;
  }

  void onPointDownImage(PointerDownEvent e) {
    debugPrint("onPointDownImage ${e.kind}");
    _stopFling = true;
    if (isDesktop) _queryOtherWindowCoords = true;
    _remoteWindowCoords = [];
    _windowRect = null;
    if (isViewOnly && !showMyCursor) return;
    if (isViewCamera) return;

    // Keep the pointer region origin in sync for mapping local -> global.
    _pointerRegionTopLeftGlobal = e.position - e.localPosition;

    if (e.kind != ui.PointerDeviceKind.mouse) {
      if (isPhysicalMouse.value) {
        isPhysicalMouse.value = false;
      }
    }
    if (isPhysicalMouse.value) {
      // In relative mouse mode, send button events without position
      if (relativeMouseMode.value) {
        _sendRelativeMouseButton(_getMouseEvent(e, _kMouseEventDown));
      } else {
        handleMouse(_getMouseEvent(e, _kMouseEventDown), e.position);
      }
    }
  }

  void onPointUpImage(PointerUpEvent e) {
    if (isDesktop) _queryOtherWindowCoords = false;
    if (isViewOnly && !showMyCursor) return;
    if (isViewCamera) return;

    // Keep the pointer region origin in sync for mapping local -> global.
    _pointerRegionTopLeftGlobal = e.position - e.localPosition;

    if (e.kind != ui.PointerDeviceKind.mouse) return;
    if (isPhysicalMouse.value) {
      // In relative mouse mode, send button events without position
      if (relativeMouseMode.value) {
        _sendRelativeMouseButton(_getMouseEvent(e, _kMouseEventUp));
      } else {
        handleMouse(_getMouseEvent(e, _kMouseEventUp), e.position);
      }
    }
  }

  static String _mouseEventTypeToPeer(String type) {
    switch (type) {
      case _kMouseEventDown:
        return kMouseEventTypeDown;
      case _kMouseEventUp:
        return kMouseEventTypeUp;
      default:
        return '';
    }
  }

  static String _mouseButtonsToPeer(int buttons) {
    // Keep consistent with processEventToPeer() mapping.
    switch (buttons) {
      case kPrimaryMouseButton:
        return 'left';
      case kSecondaryMouseButton:
        return 'right';
      case kMiddleMouseButton:
        return 'wheel';
      case kBackMouseButton:
        return 'back';
      case kForwardMouseButton:
        return 'forward';
      default:
        return '';
    }
  }

  /// Send mouse button event without position (for relative mouse mode)
  Future<void> _sendRelativeMouseButton(Map<String, dynamic> evt) async {
    if (!keyboardPerm) return;
    if (isViewCamera) return;

    final rawType = evt['type'];
    final rawButtons = evt['buttons'];
    if (rawType is! String || rawButtons is! int) return;

    final type = _mouseEventTypeToPeer(rawType);
    if (type.isEmpty) return;

    final buttons = _mouseButtonsToPeer(rawButtons);
    if (buttons.isEmpty) return;

    await bind.sessionSendMouse(
        sessionId: sessionId,
        msg: json.encode(modify({'type': type, 'buttons': buttons})));
  }

  void onPointMoveImage(PointerMoveEvent e) {
    if (isViewOnly && !showMyCursor) return;
    if (isViewCamera) return;
    if (e.kind != ui.PointerDeviceKind.mouse) return;

    // Keep the pointer region origin in sync for mapping local -> global.
    _pointerRegionTopLeftGlobal = e.position - e.localPosition;

    if (_queryOtherWindowCoords) {
      Future.delayed(Duration.zero, () async {
        _windowRect = await fillRemoteCoordsAndGetCurFrame(_remoteWindowCoords);
      });
      _queryOtherWindowCoords = false;
    }
    if (isPhysicalMouse.value) {
      if (!_handleRelativeMouseMove(e.localPosition)) {
        handleMouse(_getMouseEvent(e, _kMouseEventMove), e.position,
            edgeScroll: useEdgeScroll);
      }
    }
  }

  static Future<Rect?> fillRemoteCoordsAndGetCurFrame(
      List<RemoteWindowCoords> remoteWindowCoords) async {
    final coords =
        await rustDeskWinManager.getOtherRemoteWindowCoordsFromMain();
    final wc = WindowController.fromWindowId(kWindowId!);
    try {
      final frame = await wc.getFrame();
      for (final c in coords) {
        c.relativeOffset = Offset(
            c.windowRect.left - frame.left, c.windowRect.top - frame.top);
        remoteWindowCoords.add(c);
      }
      return frame;
    } catch (e) {
      // Unreachable code
      debugPrint("Failed to get frame of window $kWindowId, it may be hidden");
    }
    return null;
  }

  /// Handle scroll/wheel events.
  /// Note: Scroll events intentionally use absolute positioning even in relative mouse mode.
  /// This is because scroll events don't need relative positioning - they represent
  /// scroll deltas that are independent of cursor position. Games and 3D applications
  /// handle scroll events the same way regardless of mouse mode.
  void onPointerSignalImage(PointerSignalEvent e) {
    if (isViewOnly) return;
    if (isViewCamera) return;
    if (e is PointerScrollEvent) {
      var dx = e.scrollDelta.dx.toInt();
      var dy = e.scrollDelta.dy.toInt();
      if (dx > 0) {
        dx = -1;
      } else if (dx < 0) {
        dx = 1;
      }
      if (dy > 0) {
        dy = -1;
      } else if (dy < 0) {
        dy = 1;
      }
      bind.sessionSendMouse(
          sessionId: sessionId,
          msg: '{"type": "wheel", "x": "$dx", "y": "$dy"}');
    }
  }

  void refreshMousePos() => handleMouse({
        'buttons': 0,
        'type': _kMouseEventMove,
      }, lastMousePos, edgeScroll: useEdgeScroll);

  void tryMoveEdgeOnExit(Offset pos) => handleMouse(
        {
          'buttons': 0,
          'type': _kMouseEventMove,
        },
        pos,
        onExit: true,
      );

  static double tryGetNearestRange(double v, double min, double max, double n) {
    if (v < min && v >= min - n) {
      v = min;
    }
    if (v > max && v <= max + n) {
      v = max;
    }
    return v;
  }

  Offset setNearestEdge(double x, double y, Rect rect) {
    double left = x - rect.left;
    double right = rect.right - 1 - x;
    double top = y - rect.top;
    double bottom = rect.bottom - 1 - y;
    if (left < right && left < top && left < bottom) {
      x = rect.left;
    }
    if (right < left && right < top && right < bottom) {
      x = rect.right - 1;
    }
    if (top < left && top < right && top < bottom) {
      y = rect.top;
    }
    if (bottom < left && bottom < right && bottom < top) {
      y = rect.bottom - 1;
    }
    return Offset(x, y);
  }

  void handlePointerEvent(String kind, String type, Offset offset) {
    double x = offset.dx;
    double y = offset.dy;
    if (_checkPeerControlProtected(x, y)) {
      return;
    }
    // Only touch events are handled for now. So we can just ignore buttons.
    // to-do: handle mouse events

    late final dynamic evtValue;
    if (type == kMouseEventTypePanUpdate) {
      evtValue = {
        'x': x.toInt(),
        'y': y.toInt(),
      };
    } else {
      final isMoveTypes = [kMouseEventTypePanStart, kMouseEventTypePanEnd];
      final pos = handlePointerDevicePos(
        kPointerEventKindTouch,
        x,
        y,
        isMoveTypes.contains(type),
        type,
      );
      if (pos == null) {
        return;
      }
      evtValue = {
        'x': pos.x.toInt(),
        'y': pos.y.toInt(),
      };
    }

    final evt = PointerEventToRust(kind, type, evtValue).toJson();
    if (isViewCamera) return;
    bind.sessionSendPointer(
        sessionId: sessionId, msg: json.encode(modify(evt)));
  }

  bool _checkPeerControlProtected(double x, double y) {
    final cursorModel = parent.target!.cursorModel;
    if (cursorModel.isPeerControlProtected) {
      lastMousePos = ui.Offset(x, y);
      return true;
    }

    if (!cursorModel.gotMouseControl) {
      bool selfGetControl =
          (x - lastMousePos.dx).abs() > kMouseControlDistance ||
              (y - lastMousePos.dy).abs() > kMouseControlDistance;
      if (selfGetControl) {
        cursorModel.gotMouseControl = true;
      } else {
        lastMousePos = ui.Offset(x, y);
        return true;
      }
    }
    lastMousePos = ui.Offset(x, y);
    return false;
  }

  Map<String, dynamic>? processEventToPeer(
    Map<String, dynamic> evt,
    Offset offset, {
    bool onExit = false,
    bool moveCanvas = true,
    bool edgeScroll = false,
  }) {
    if (isViewCamera) return null;
    double x = offset.dx;
    double y = max(0.0, offset.dy);
    if (_checkPeerControlProtected(x, y)) {
      return null;
    }

    var type = kMouseEventTypeDefault;
    var isMove = false;
    switch (evt['type']) {
      case _kMouseEventDown:
        type = kMouseEventTypeDown;
        break;
      case _kMouseEventUp:
        type = kMouseEventTypeUp;
        break;
      case _kMouseEventMove:
        _pointerMovedAfterEnter = true;
        isMove = true;
        break;
      default:
        return null;
    }
    evt['type'] = type;

    if (type == kMouseEventTypeDown && !_pointerMovedAfterEnter) {
      // Move mouse to the position of the down event first.
      lastMousePos = ui.Offset(x, y);
      refreshMousePos();
    }

    final pos = handlePointerDevicePos(
      kPointerEventKindMouse,
      x,
      y,
      isMove,
      type,
      onExit: onExit,
      buttons: evt['buttons'],
      moveCanvas: moveCanvas,
      edgeScroll: edgeScroll,
    );
    if (pos == null) {
      return null;
    }
    if (type != '') {
      evt['x'] = '0';
      evt['y'] = '0';
    } else {
      evt['x'] = '${pos.x.toInt()}';
      evt['y'] = '${pos.y.toInt()}';
    }

    final buttons = evt['buttons'];
    if (buttons is int) {
      evt['buttons'] = _mouseButtonsToPeer(buttons);
    } else {
      evt['buttons'] = '';
    }
    return evt;
  }

  Map<String, dynamic>? handleMouse(
    Map<String, dynamic> evt,
    Offset offset, {
    bool onExit = false,
    bool moveCanvas = true,
    bool edgeScroll = false,
  }) {
    final evtToPeer =
        processEventToPeer(evt, offset, onExit: onExit, moveCanvas: moveCanvas, edgeScroll: edgeScroll);
    if (evtToPeer != null) {
      bind.sessionSendMouse(
          sessionId: sessionId, msg: json.encode(modify(evtToPeer)));
    }
    return evtToPeer;
  }

  Point? handlePointerDevicePos(
    String kind,
    double x,
    double y,
    bool isMove,
    String evtType, {
    bool onExit = false,
    int buttons = kPrimaryMouseButton,
    bool moveCanvas = true,
    bool edgeScroll = false,
  }) {
    final ffiModel = parent.target!.ffiModel;
    CanvasCoords canvas =
        CanvasCoords.fromCanvasModel(parent.target!.canvasModel);
    Rect? rect = ffiModel.rect;

    if (isMove) {
      if (_remoteWindowCoords.isNotEmpty &&
          _windowRect != null &&
          !_isInCurrentWindow(x, y)) {
        final coords =
            findRemoteCoords(x, y, _remoteWindowCoords, devicePixelRatio);
        if (coords != null) {
          isMove = false;
          canvas = coords.canvas;
          rect = coords.remoteRect;
          x -= isWindows
              ? coords.relativeOffset.dx / devicePixelRatio
              : coords.relativeOffset.dx;
          y -= isWindows
              ? coords.relativeOffset.dy / devicePixelRatio
              : coords.relativeOffset.dy;
        }
      }
    }

    y -= CanvasModel.topToEdge;
    x -= CanvasModel.leftToEdge;
    if (isMove) {
      final canvasModel = parent.target!.canvasModel;

      if (edgeScroll) {
        canvasModel.edgeScrollMouse(x, y);
      } else if (moveCanvas) {
        canvasModel.moveDesktopMouse(x, y);
      }

      canvasModel.updateLocalCursor(x, y);
    }

    return _handlePointerDevicePos(
      kind,
      x,
      y,
      isMove,
      canvas,
      rect,
      evtType,
      onExit: onExit,
      buttons: buttons,
    );
  }

  bool _isInCurrentWindow(double x, double y) {
    var w = _windowRect!.width;
    var h = _windowRect!.height;
    if (isWindows) {
      w /= devicePixelRatio;
      h /= devicePixelRatio;
    }
    return x >= 0 && y >= 0 && x <= w && y <= h;
  }

  static RemoteWindowCoords? findRemoteCoords(double x, double y,
      List<RemoteWindowCoords> remoteWindowCoords, double devicePixelRatio) {
    if (isWindows) {
      x *= devicePixelRatio;
      y *= devicePixelRatio;
    }
    for (final c in remoteWindowCoords) {
      if (x >= c.relativeOffset.dx &&
          y >= c.relativeOffset.dy &&
          x <= c.relativeOffset.dx + c.windowRect.width &&
          y <= c.relativeOffset.dy + c.windowRect.height) {
        return c;
      }
    }
    return null;
  }

  Point? _handlePointerDevicePos(
    String kind,
    double x,
    double y,
    bool moveInCanvas,
    CanvasCoords canvas,
    Rect? rect,
    String evtType, {
    bool onExit = false,
    int buttons = kPrimaryMouseButton,
  }) {
    if (rect == null) {
      return null;
    }

    final nearThr = 3;
    var nearRight = (canvas.size.width - x) < nearThr;
    var nearBottom = (canvas.size.height - y) < nearThr;
    final imageWidth = rect.width * canvas.scale;
    final imageHeight = rect.height * canvas.scale;
    if (canvas.scrollStyle != ScrollStyle.scrollauto) {
      x += imageWidth * canvas.scrollX;
      y += imageHeight * canvas.scrollY;

      // boxed size is a center widget
      if (canvas.size.width > imageWidth) {
        x -= ((canvas.size.width - imageWidth) / 2);
      }
      if (canvas.size.height > imageHeight) {
        y -= ((canvas.size.height - imageHeight) / 2);
      }
    } else {
      x -= canvas.x;
      y -= canvas.y;
    }

    x /= canvas.scale;
    y /= canvas.scale;
    if (canvas.scale > 0 && canvas.scale < 1) {
      final step = 1.0 / canvas.scale - 1;
      if (nearRight) {
        x += step;
      }
      if (nearBottom) {
        y += step;
      }
    }
    x += rect.left;
    y += rect.top;

    if (onExit) {
      final pos = setNearestEdge(x, y, rect);
      x = pos.dx;
      y = pos.dy;
    }

    return InputModel.getPointInRemoteRect(
        true, peerPlatform, kind, evtType, x, y, rect,
        buttons: buttons);
  }

  static Point<double>? getPointInRemoteRect(
      bool isLocalDesktop,
      String? peerPlatform,
      String kind,
      String evtType,
      double evtX,
      double evtY,
      Rect rect,
      {int buttons = kPrimaryMouseButton}) {
    double minX = rect.left;
    // https://github.com/rustdesk/rustdesk/issues/6678
    // For Windows, [0,maxX], [0,maxY] should be set to enable window snapping.
    double maxX = (rect.left + rect.width) -
        (peerPlatform == kPeerPlatformWindows ? 0 : 1);
    double minY = rect.top;
    double maxY = (rect.top + rect.height) -
        (peerPlatform == kPeerPlatformWindows ? 0 : 1);
    evtX = InputModel.tryGetNearestRange(evtX, minX, maxX, 5);
    evtY = InputModel.tryGetNearestRange(evtY, minY, maxY, 5);
    if (isLocalDesktop) {
      if (kind == kPointerEventKindMouse) {
        if (evtX < minX || evtY < minY || evtX > maxX || evtY > maxY) {
          // If left mouse up, no early return.
          if (!(buttons == kPrimaryMouseButton &&
              evtType == kMouseEventTypeUp)) {
            return null;
          }
        }
      }
    } else {
      bool evtXInRange = evtX >= minX && evtX <= maxX;
      bool evtYInRange = evtY >= minY && evtY <= maxY;
      if (!(evtXInRange || evtYInRange)) {
        return null;
      }
      if (evtX < minX) {
        evtX = minX;
      } else if (evtX > maxX) {
        evtX = maxX;
      }
      if (evtY < minY) {
        evtY = minY;
      } else if (evtY > maxY) {
        evtY = maxY;
      }
    }

    return Point(evtX, evtY);
  }

  /// Web only
  void listenToMouse(bool yesOrNo) {
    if (yesOrNo) {
      platformFFI.startDesktopWebListener();
    } else {
      platformFFI.stopDesktopWebListener();
    }
  }

  void onMobileBack() {
    final minBackButtonVersion = "1.3.8";
    final peerVersion =
        parent.target?.ffiModel.pi.version ?? minBackButtonVersion;
    var btn = MouseButtons.back;
    // For compatibility with old versions
    if (versionCmp(peerVersion, minBackButtonVersion) < 0) {
      btn = MouseButtons.right;
    }
    tap(btn);
  }

  void onMobileHome() => tap(MouseButtons.wheel);
  Future<void> onMobileApps() async {
    sendMouse('down', MouseButtons.wheel);
    await Future.delayed(const Duration(milliseconds: 500));
    sendMouse('up', MouseButtons.wheel);
  }

  // Simulate a key press event.
  // `usbHidUsage` is the USB HID usage code of the key.
  Future<void> tapHidKey(int usbHidUsage) async {
    newKeyboardMode(kKeyFlutterKey, usbHidUsage, true);
    await Future.delayed(Duration(milliseconds: 100));
    newKeyboardMode(kKeyFlutterKey, usbHidUsage, false);
  }

  Future<void> onMobileVolumeUp() async =>
      await tapHidKey(PhysicalKeyboardKey.audioVolumeUp.usbHidUsage & 0xFFFF);
  Future<void> onMobileVolumeDown() async =>
      await tapHidKey(PhysicalKeyboardKey.audioVolumeDown.usbHidUsage & 0xFFFF);
  Future<void> onMobilePower() async =>
      await tapHidKey(PhysicalKeyboardKey.power.usbHidUsage & 0xFFFF);
}
