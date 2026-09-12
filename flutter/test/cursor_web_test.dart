// The test drives the cursor lifecycle normally owned by MouseTracker.
// ignore_for_file: invalid_use_of_protected_member

@TestOn('browser')
library;

import 'dart:convert';
import 'dart:js' as js;
import 'dart:typed_data';
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

class _CursorModel extends Fake implements model.CursorModel {
  @override
  final Set<String> cachedKeys = {};
  @override
  void addKey(String key) => cachedKeys.add(key);
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  for (final style in [kRemoteViewStyleAdaptive, kRemoteViewStyleCustom]) {
    for (final dpr in [1.0, 2.0]) {
      testWidgets('ImagePaint Web $style zoom off DPR $dpr keeps source size',
          (tester) => tester.runAsync(() => _checkPolicy(tester, style, dpr)));
    }
  }
  test('Web cursor aligns CSS hotspots with rounded PNG dimensions', () async {
    Map<String, dynamic>? registered;
    final original = js.context['setByName'];
    js.context['setByName'] = js.allowInterop((String name, String value) {
      expect(name, 'cursor');
      registered = jsonDecode(value) as Map<String, dynamic>;
    });
    addTearDown(() => js.context['setByName'] = original);
    final nativeImage = await createTestImage(width: 48, height: 48);
    addTearDown(nativeImage.dispose);
    for (final (hotspot, scale, side, expected) in [
      ((7.0, 7.0), 633 / 1600, 19, (3, 3)),
      ((21.0, 23.0), 633 / 1600, 19, (8, 9)),
      ((22.0, 22.0), 633 / 1600, 19, (9, 9)),
      ((21.0, 23.0), 0.05, 12, (5, 6)),
      ((21.0, 23.0), 0.5, 24, (11, 12)),
      ((7.0, 7.0), 1.0, 48, (7, 7)),
    ]) {
      final cache = _data(nativeImage, hotspot);
      final session =
          buildCursorOfCache(_CursorModel(), scale, cache).createSession(1);
      await session.activate();
      final uri = Uri.parse(registered!['url'] as String);
      final bitmap = img.decodePng(uri.data!.contentAsBytes())!;
      expect((bitmap.width, bitmap.height), (side, side));
      expect((registered!['hotx'], registered!['hoty']), expected);
      session.dispose();
      await deleteCustomCursor(cache.updateGetKey(scale));
    }
  });
}

model.CursorData _data(ui.Image nativeImage, (double, double) hotspot) {
  final image = img.Image(width: 48, height: 48, numChannels: 4);
  img.fill(image, color: img.ColorRgba8(255, 255, 255, 255));
  return model.CursorData(
    peerId: 'web-cursor-test',
    id: '$hotspot',
    image: image,
    nativeImage: nativeImage,
    scale: 1,
    data: Uint8List.fromList(img.encodePng(image)),
    hotxOrigin: hotspot.$1,
    hotyOrigin: hotspot.$2,
    width: 48,
    height: 48,
  );
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

Future<model.CursorModel> _loadCursor(model.FFI ffi, String id) async {
  final cursor = model.CursorModel(WeakReference(ffi))..id = id;
  await cursor.updateCursorData({
    'id': id,
    'width': '48',
    'height': '48',
    'hotx': '7',
    'hoty': '9',
    'scale': '1',
    'colors': jsonEncode(List.filled(48 * 48 * 4, 255)),
  });
  addTearDown(() async {
    for (final key in cursor.cachedKeys) {
      await deleteCustomCursor(key);
    }
    cursor.disposeImages();
    cursor.dispose();
  });
  return cursor;
}

Future<void> _checkPolicy(WidgetTester tester, String style, double dpr) async {
  final originals = {
    for (final key in ['isMobile', 'getByName', 'setByName'])
      key: js.context[key]
  };
  js.context['isMobile'] = js.allowInterop(() => false);
  js.context['getByName'] = js.allowInterop((String name, String value) => '');
  Map<String, dynamic>? registered;
  js.context['setByName'] = js.allowInterop((String name, String value) {
    if (name == 'cursor') registered = jsonDecode(value);
  });
  addTearDown(() {
    for (final entry in originals.entries) {
      js.context[entry.key] = entry.value;
    }
  });
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
      img.decodePng(Uri.parse(registered!['url']).data!.contentAsBytes())!;
  expect((png.width, png.height), (48, 48));
  expect((registered!['hotx'], registered!['hoty']), (7, 9));
}
