import 'package:flutter/gestures.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_hbb/common/widgets/remote_input.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/models/input_model.dart';
import 'package:flutter_hbb/models/model.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:uuid/uuid.dart';

const _position = Offset(100, 100);
const _outside = Offset(-100, -100);
const _size = Size(400, 300);
const _buttons = {
  kPrimaryMouseButton: 'left',
  kSecondaryMouseButton: 'right',
  kMiddleMouseButton: 'wheel',
  kBackMouseButton: 'back',
  kForwardMouseButton: 'forward',
};

class _Canvas extends Fake implements CanvasModel {
  @override
  double get x => 0;
  @override
  double get y => 0;
  @override
  double get scale => 1;
  @override
  double get scrollX => 0;
  @override
  double get scrollY => 0;
  @override
  Size get size => _size;
  @override
  ScrollStyle get scrollStyle => ScrollStyle.scrollauto;
  @override
  void moveDesktopMouse(double x, double y) {}
  @override
  void updateLocalCursor(double x, double y) {}
}

class _Peer extends Fake implements FfiModel {
  @override
  final pi = PeerInfo()..platform = kPeerPlatformWindows;
  @override
  bool viewOnly = false;
  @override
  bool get showMyCursor => false;
  @override
  bool get keyboard => true;
  @override
  Rect get rect => Offset.zero & _size;
}

class _Cursor extends Fake implements CursorModel {
  @override
  bool isPeerControlProtected = false;
  @override
  bool gotMouseControl = true;
}

class _FFI extends Fake implements FFI {
  @override
  final sessionId = UuidValue('00000000-0000-4000-8000-000000000005');
  @override
  String get id => '';
  @override
  ConnType get connType => ConnType.defaultConn;
  @override
  final _Peer ffiModel = _Peer();
  @override
  final _Canvas canvasModel = _Canvas();
  @override
  final _Cursor cursorModel = _Cursor();
  @override
  late final _Input inputModel = _Input(WeakReference(this));
}

class _Input extends InputModel {
  _Input(super.parent);
  final messages = <Map<String, dynamic>>[];

  List<(String, String)> get buttonEvents => messages
      .where((event) => event['type'] == 'down' || event['type'] == 'up')
      .map((event) => (event['type'] as String, event['buttons'] as String))
      .toList();

  @override
  Map<String, dynamic>? handleMouse(Map<String, dynamic> evt, Offset offset,
      {bool onExit = false, bool moveCanvas = true, bool edgeScroll = false}) {
    final result = processEventToPeer(evt, offset,
        onExit: onExit, moveCanvas: moveCanvas, edgeScroll: edgeScroll);
    if (result != null) messages.add(Map.of(result));
    return result;
  }

  @override
  Future<void> sendMouse(String type, MouseButtons button) async {
    messages.add({'type': type, 'buttons': button.value});
  }
}

PointerDownEvent _down(int buttons) => PointerDownEvent(
    kind: PointerDeviceKind.mouse, position: _position, buttons: buttons);

const _up = PointerUpEvent(kind: PointerDeviceKind.mouse, position: _position);
const _hover =
    PointerHoverEvent(kind: PointerDeviceKind.mouse, position: _position);
const _cancel =
    PointerCancelEvent(kind: PointerDeviceKind.mouse, position: _outside);

void _testProtectedReleases(_FFI Function() fixture) {
  for (final protected in [true, false]) {
    for (final button in _buttons.entries) {
      test('${button.value} release survives protected=$protected', () {
        final ffi = fixture();
        final input = ffi.inputModel;
        input.onPointDownImage(_down(button.key));
        ffi.cursorModel.isPeerControlProtected = protected;
        ffi.cursorModel.gotMouseControl = false;
        input.onPointUpImage(_up);
        ffi.cursorModel.isPeerControlProtected = false;
        ffi.cursorModel.gotMouseControl = true;
        input.onPointHoverImage(_hover);
        expect(
            input.buttonEvents, [('down', button.value), ('up', button.value)]);
        expect(input.messages.last['type'], kMouseEventTypeDefault);
      });
    }
  }
  test('hover recovers a missed up even during peer protection', () {
    final ffi = fixture();
    ffi.inputModel.onPointDownImage(_down(kPrimaryMouseButton));
    ffi.cursorModel.isPeerControlProtected = true;
    ffi.inputModel.onPointHoverImage(_hover);
    expect(ffi.inputModel.buttonEvents, [('down', 'left'), ('up', 'left')]);
  });
  test('peer protection still blocks new presses and movement', () {
    final ffi = fixture();
    ffi.cursorModel.isPeerControlProtected = true;
    ffi.inputModel.onPointDownImage(_down(kPrimaryMouseButton));
    ffi.inputModel.refreshMousePos();
    expect(ffi.inputModel.messages, isEmpty);
  });
}

void _testOutsideReleases(_FFI Function() fixture) {
  for (final button in _buttons.entries) {
    test('${button.value} release survives leaving the image', () {
      final input = fixture().inputModel;
      input.onPointDownImage(_down(button.key));
      input.onPointUpImage(const PointerUpEvent(
          kind: PointerDeviceKind.mouse, position: _outside));
      expect(
          input.buttonEvents, [('down', button.value), ('up', button.value)]);
    });
  }
}

Future<Listener> _mountRegion(WidgetTester tester, _Input input) async {
  await tester.pumpWidget(Directionality(
    textDirection: TextDirection.ltr,
    child: RawPointerMouseRegion(
        inputModel: input, child: const SizedBox.expand()),
  ));
  return tester.widget<Listener>(find.descendant(
      of: find.byType(RawPointerMouseRegion), matching: find.byType(Listener)));
}

void _testCancellation(_FFI Function() fixture) {
  for (final button in _buttons.entries) {
    testWidgets('${button.value} cancellation releases outside the image',
        (tester) async {
      final input = fixture().inputModel;
      final listener = await _mountRegion(tester, input);
      listener.onPointerDown!(_down(button.key));
      listener.onPointerCancel?.call(_cancel);
      expect(
          input.buttonEvents, [('down', button.value), ('up', button.value)]);
      listener.onPointerCancel?.call(_cancel);
      input.onPointHoverImage(_hover);
      expect(
          input.buttonEvents, [('down', button.value), ('up', button.value)]);
    });
  }
  testWidgets('cancel without a mouse press sends no release', (tester) async {
    final input = fixture().inputModel;
    final listener = await _mountRegion(tester, input);
    listener.onPointerCancel?.call(_cancel);
    expect(input.buttonEvents, isEmpty);
  });
  testWidgets('touch cancellation does not release the mouse', (tester) async {
    final input = fixture().inputModel;
    final listener = await _mountRegion(tester, input);
    listener.onPointerDown!(_down(kPrimaryMouseButton));
    listener.onPointerCancel
        ?.call(const PointerCancelEvent(kind: PointerDeviceKind.touch));
    expect(input.buttonEvents, [('down', 'left')]);
    listener.onPointerUp!(_up);
    expect(input.buttonEvents, [('down', 'left'), ('up', 'left')]);
  });
}

void _testTrackedCancellation(_FFI Function() fixture) {
  testWidgets('a blocked press does not release another controller button',
      (tester) async {
    final ffi = fixture();
    final listener = await _mountRegion(tester, ffi.inputModel);
    ffi.cursorModel.isPeerControlProtected = true;
    listener.onPointerDown!(_down(kPrimaryMouseButton));
    listener.onPointerUp!(_up);
    listener.onPointerDown!(_down(kPrimaryMouseButton));
    listener.onPointerCancel?.call(_cancel);
    expect(ffi.inputModel.messages, isEmpty);
  });
  for (final relative in [false, true]) {
    testWidgets('cancel releases each recorded button with relative=$relative',
        (tester) async {
      final input = fixture().inputModel;
      final listener = await _mountRegion(tester, input);
      listener.onPointerDown!(_down(kPrimaryMouseButton));
      input.handleMouse(
          {'type': 'mousedown', 'buttons': kSecondaryMouseButton}, _position);
      input.relativeMouseMode.value = relative;
      listener.onPointerCancel?.call(_cancel);
      expect(input.buttonEvents, [
        ('down', 'left'),
        ('down', 'right'),
        ('up', 'left'),
        ('up', 'right')
      ]);
      input.relativeMouseMode.value = false;
    });
  }
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  late _FFI ffi;
  setUp(() => ffi = _FFI());
  tearDown(() => ffi.inputModel.relativeMouseMode.close());
  _testProtectedReleases(() => ffi);
  _testOutsideReleases(() => ffi);
  _testCancellation(() => ffi);
  _testTrackedCancellation(() => ffi);
  test('moving to the image edge preserves a normal drag', () {
    final input = ffi.inputModel;
    input.onPointDownImage(_down(kPrimaryMouseButton));
    input.tryMoveEdgeOnExit(_outside);
    expect(input.buttonEvents, [('down', 'left')]);
    input.onPointUpImage(_up);
    expect(input.buttonEvents, [('down', 'left'), ('up', 'left')]);
  });
}
