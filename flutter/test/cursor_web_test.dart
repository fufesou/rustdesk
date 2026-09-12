// The test drives the cursor lifecycle normally owned by MouseTracker.
// ignore_for_file: invalid_use_of_protected_member

@TestOn('browser')
library;

import 'dart:convert';
import 'dart:js' as js;
import 'dart:ui' as ui;

import 'package:flutter/widgets.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/desktop/pages/remote_page.dart';
import 'package:flutter_hbb/models/input_model.dart';
import 'package:flutter_hbb/models/model.dart' as model;
import 'package:flutter_hbb/web/custom_cursor.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:get/get.dart';
import 'package:image/image.dart' as img;
import 'package:provider/provider.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  _alphaTests();
  for (final style in [kRemoteViewStyleAdaptive, kRemoteViewStyleCustom]) {
    for (final dpr in [1.0, 2.0]) {
      testWidgets('ImagePaint Web $style zoom off DPR $dpr keeps source size',
          (tester) => tester.runAsync(() => _checkPolicy(tester, style, dpr)));
    }
  }
  test('Web cursor aligns CSS hotspots with rounded PNG dimensions', () async {
    final registered = _captureCursor();
    final canvas = _Canvas(kRemoteViewStyleAdaptive);
    addTearDown(canvas.dispose);
    final ffi = _FFI(canvas);
    for (final (hotspot, scale, side, expected) in [
      ((7.0, 7.0), 633 / 1600, 19, (3, 3)),
      ((21.0, 23.0), 633 / 1600, 19, (8, 9)),
      ((22.0, 22.0), 633 / 1600, 19, (9, 9)),
      ((21.0, 23.0), 0.05, 12, (5, 6)),
      ((21.0, 23.0), 0.5, 24, (11, 12)),
      ((7.0, 7.0), 1.0, 48, (7, 7)),
    ]) {
      final cursor = await _loadCursor(ffi, '$hotspot-$scale',
          hotspot: hotspot, pixelRatio: null);
      final session =
          buildCursorOfCache(cursor, scale, cursor.cache).createSession(1);
      await session.activate();
      final uri = Uri.parse(registered['url'] as String);
      final bitmap = img.decodePng(uri.data!.contentAsBytes())!;
      expect((bitmap.width, bitmap.height), (side, side));
      expect((registered['hotx'], registered['hoty']), expected);
      session.dispose();
    }
  });
}

class _Image extends ChangeNotifier implements model.ImageModel {
  @override
  bool get useTextureRender => false;
  @override
  ui.Image? get image => null;
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _Canvas extends ChangeNotifier implements model.CanvasModel {
  _Canvas(String style)
      : viewStyle = model.ViewStyle(
            style: style,
            width: 200,
            height: 160,
            displayWidth: 400,
            displayHeight: 320);
  @override
  final model.ViewStyle viewStyle;
  @override
  final imageOverflow = false.obs;
  @override
  bool get cursorEmbedded => false;
  @override
  Size get size => const Size(200, 160);
  @override
  double get scale => 0.5;
  @override
  double get x => 0;
  @override
  double get y => 0;
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _Input extends Fake implements InputModel {
  @override
  final relativeMouseMode = false.obs;
}

class _Peer extends Fake implements model.FfiModel {
  @override
  final pi = model.PeerInfo();
  @override
  bool get isPeerLinux => false;
}

class _FFI extends Fake implements model.FFI {
  _FFI(this.canvasModel);
  @override
  final model.CanvasModel canvasModel;
  @override
  final inputModel = _Input();
  @override
  final ffiModel = _Peer();
}

Future<model.CursorModel> _loadCursor(model.FFI ffi, String id,
    {List<int> pixel = const [255, 255, 255, 255],
    (double, double) hotspot = (7, 9),
    double? pixelRatio = 1}) async {
  final cursor = model.CursorModel(WeakReference(ffi))..id = id;
  await cursor.updateCursorData({
    'id': id,
    'width': '48',
    'height': '48',
    'hotx': '${hotspot.$1}',
    'hoty': '${hotspot.$2}',
    if (pixelRatio != null) 'scale': '$pixelRatio',
    'colors': jsonEncode([for (var i = 0; i < 48 * 48; i++) ...pixel]),
  });
  addTearDown(() async {
    // Keep the session owner alive across asynchronous image decoding.
    expect(cursor.parent.target, same(ffi));
    for (final key in cursor.cachedKeys) {
      await deleteCustomCursor(key);
    }
    cursor.disposeImages();
    cursor.dispose();
  });
  return cursor;
}

Map<String, dynamic> _captureCursor() {
  final originals = {
    for (final key in ['isMobile', 'getByName', 'setByName'])
      key: js.context[key]
  };
  js.context['isMobile'] = js.allowInterop(() => false);
  js.context['getByName'] = js.allowInterop((String name, String value) => '');
  final registered = <String, dynamic>{};
  js.context['setByName'] = js.allowInterop((String name, String value) {
    if (name == 'cursor') {
      registered
        ..clear()
        ..addAll(jsonDecode(value));
    }
  });
  addTearDown(() {
    for (final entry in originals.entries) {
      js.context[entry.key] = entry.value;
    }
  });
  return registered;
}

Future<void> _checkPolicy(WidgetTester tester, String style, double dpr) async {
  final registered = _captureCursor();
  final canvas = _Canvas(style);
  addTearDown(canvas.dispose);
  final ffi = _FFI(canvas);
  final cursor = await _loadCursor(ffi, '$style-$dpr');
  await tester.pumpWidget(MediaQuery(
    data: MediaQueryData(devicePixelRatio: dpr),
    child: MultiProvider(
        providers: [
          ChangeNotifierProvider<model.ImageModel>(create: (_) => _Image()),
          ChangeNotifierProvider<model.CanvasModel>.value(value: canvas),
          ChangeNotifierProvider<model.CursorModel>.value(value: cursor),
        ],
        child: ImagePaint(
            ffi: ffi,
            id: 'web-cursor-test',
            zoomCursor: false.obs,
            cursorOverImage: true.obs,
            keyboardEnabled: true.obs,
            remoteCursorMoved: false.obs)),
  ));
  final session = tester
      .widget<MouseRegion>(find.byType(MouseRegion))
      .cursor
      .createSession(1);
  await session.activate();
  session.dispose();
  await tester.pumpWidget(const SizedBox.shrink());
  final png =
      img.decodePng(Uri.parse(registered['url']).data!.contentAsBytes())!;
  expect((png.width, png.height), (48, 48));
  expect((registered['hotx'], registered['hoty']), (7, 9));
}

void _alphaTests() {
  for (final (density, pixel) in [
    (1.0, [255, 255, 255, 128]),
    (2.0, [255, 128, 64, 128]),
    (1.0, [255, 128, 64, 112]),
    (1.0, [0, 0, 0, 0]),
    (1.0, [255, 255, 255, 255]),
    // Old macOS hosts send straight alpha without density metadata.
    (0.0, [255, 128, 64, 128]),
    (null, [80, 40, 20, 128]),
  ]) {
    test('Web macOS cursor density $density preserves RGBA $pixel',
        () => _checkAlpha(density, pixel));
  }
}

Future<void> _checkAlpha(double? density, List<int> pixel) async {
  final registered = _captureCursor();
  final canvas = _Canvas(kRemoteViewStyleAdaptive);
  addTearDown(canvas.dispose);
  final ffi = _FFI(canvas)..ffiModel.pi.platform = kPeerPlatformMacOS;
  final cursor = await _loadCursor(ffi, 'alpha-$density-$pixel',
      pixel: pixel, pixelRatio: density);
  // Cover both the painted remote cursor and the initial CSS cursor PNG.
  final straight = await cursor.image!
      .toByteData(format: ui.ImageByteFormat.rawStraightRgba);
  expect(straight!.buffer.asUint8List(0, 4), pixel);
  for (final scale in [1.0, 0.5, 1.0]) {
    final session =
        buildCursorOfCache(cursor, scale, cursor.cache).createSession(1);
    await session.activate();
    session.dispose();
    final png =
        img.decodePng(Uri.parse(registered['url']).data!.contentAsBytes())!;
    final color = png.getPixel(png.width ~/ 2, png.height ~/ 2);
    expect([color.r, color.g, color.b, color.a], pixel);
  }
}
