import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/desktop/pages/remote_page.dart';
import 'package:flutter_hbb/models/input_model.dart';
import 'package:flutter_hbb/models/model.dart';
import 'package:flutter_hbb/native/custom_cursor.dart' show deleteCustomCursor;
import 'package:flutter_test/flutter_test.dart';
import 'package:get/get.dart';
import 'package:image/image.dart' as img;
import 'package:provider/provider.dart';

const _viewport = Size(200, 160);

class _Image extends ChangeNotifier implements ImageModel {
  @override
  bool get useTextureRender => false;
  @override
  ui.Image? get image => null;
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _Canvas extends ChangeNotifier implements CanvasModel {
  _Canvas(this.devicePixelRatio, {required this.style, required this.scale});

  final String style;

  @override
  final double devicePixelRatio;
  @override
  final imageOverflow = false.obs;
  @override
  late final viewStyle = ViewStyle(
    style: style,
    width: _viewport.width,
    height: _viewport.height,
    displayWidth: 400,
    displayHeight: 320,
  );
  @override
  bool get cursorEmbedded => false;
  @override
  Size get size => _viewport;
  @override
  final double scale;
  @override
  double get x => 0;
  @override
  double get y => 0;
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _Cursor extends ChangeNotifier implements CursorModel {
  _Cursor(this.cache, this._ffi);

  final FFI _ffi;
  @override
  WeakReference<FFI> get parent => WeakReference(_ffi);

  @override
  CursorData cache;
  @override
  ui.Image? get image => null;
  @override
  double get hotx => cache.hotxOrigin;
  @override
  double get hoty => cache.hotyOrigin;
  @override
  final Set<String> cachedKeys = {};
  @override
  void addKey(String key) => cachedKeys.add(key);
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _Input extends Fake implements InputModel {
  @override
  final relativeMouseMode = false.obs;
}

class _Peer extends Fake implements FfiModel {
  @override
  final pi = PeerInfo();
  @override
  bool get isPeerLinux => false;
}

class _FFI extends Fake implements FFI {
  _FFI(this.canvasModel);

  @override
  final CanvasModel canvasModel;
  @override
  final inputModel = _Input();
  @override
  final ffiModel = _Peer();
}

Future<CursorData> _data(int density, String id) async {
  final bitmapDensity = density == 0 ? 1 : density;
  final image = await createTestImage(
      width: 9 * bitmapDensity, height: 18 * bitmapDensity);
  return CursorData(
    peerId: 'dpi-policy',
    id: id,
    image: img.Image(width: image.width, height: image.height, numChannels: 4),
    nativeImage: image,
    scale: 1,
    data: null,
    hotxOrigin: 4.0 * bitmapDensity,
    hotyOrigin: 9.0 * bitmapDensity,
    width: image.width,
    height: image.height,
    pixelRatio: density.toDouble(),
  );
}

void main() {
  for (final (style, zoom, density, viewScale, expectedScale) in [
    (kRemoteViewStyleAdaptive, false, 0, 0.25, Platform.isWindows ? 1.0 : 4 / 3),
    (kRemoteViewStyleCustom, false, 0, 0.25, Platform.isWindows ? 1.0 : 4 / 3),
    (kRemoteViewStyleAdaptive, false, 1, 0.25, Platform.isWindows ? 2.0 : 1.0),
    (kRemoteViewStyleAdaptive, false, 2, 0.25, Platform.isWindows ? 1.0 : 0.5),
    (kRemoteViewStyleCustom, false, 2, 0.25, Platform.isWindows ? 1.0 : 0.5),
    (kRemoteViewStyleAdaptive, true, 2, 0.25, Platform.isWindows ? 2 / 3 : 1 / 3),
    (kRemoteViewStyleCustom, true, 2, 0.25, Platform.isWindows ? 2 / 3 : 1 / 3),
    (kRemoteViewStyleOriginal, false, 2, 0.5, Platform.isWindows ? 1.0 : 2 / 3),
  ]) {
    testWidgets(
        '$style zoom=$zoom peerDPR=$density keeps cursor units',
        (tester) => tester.runAsync(() async {
              tester.view.devicePixelRatio = 2;
              addTearDown(tester.view.resetDevicePixelRatio);
              final channel = Platform.isWindows
                  ? SystemChannels.mouseCursor
                  : const MethodChannel('flutter_custom_cursor');
              tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
                  channel,
                  (call) async => call.method.startsWith('createCustomCursor')
                      ? (call.arguments as Map<dynamic, dynamic>)['name']
                      : null);
              final data = await _data(density, '$style-$zoom-$density');
              // A stale cached DPR must not affect the cursor when the window moves.
              final canvas = _Canvas(1, style: style, scale: viewScale);
              final ffi = _FFI(canvas);
              final cursor = _Cursor(data, ffi);
              await tester.pumpWidget(MediaQuery(
                data: const MediaQueryData(devicePixelRatio: 2),
                child: MultiProvider(
                    providers: [
                      ChangeNotifierProvider<ImageModel>(
                          create: (_) => _Image()),
                      ChangeNotifierProvider<CanvasModel>.value(value: canvas),
                      ChangeNotifierProvider<CursorModel>.value(value: cursor),
                    ],
                    child: ImagePaint(
                      ffi: ffi,
                      id: 'dpi-policy',
                      zoomCursor: zoom.obs,
                      cursorOverImage: true.obs,
                      keyboardEnabled: true.obs,
                      remoteCursorMoved: false.obs,
                    )),
              ));
              await tester.pumpWidget(const SizedBox.shrink());
              await Future.wait(cursor.cachedKeys.map((key) async {
                await deleteCustomCursor(key);
              }));
              data.nativeImage.dispose();
              cursor.dispose();
              canvas.dispose();
              tester.binding.defaultBinaryMessenger
                  .setMockMethodCallHandler(channel, null);
              expect(data.scale, closeTo(expectedScale, 1e-9));
              expect(data.hotx, closeTo(data.hotxOrigin * expectedScale, 1e-9));
              expect(data.hoty, closeTo(data.hotyOrigin * expectedScale, 1e-9));
            }));
  }
}
