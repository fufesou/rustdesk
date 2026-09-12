import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/widgets.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/desktop/pages/remote_page.dart';
import 'package:flutter_hbb/models/model.dart';
import 'package:flutter_hbb/utils/image.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:get/get.dart';
import 'package:provider/provider.dart';

const _hotspot = Offset(4, 9);
const _remotePosition = Offset(100.25, 80.75);
const _canvasOffset = Offset(15.125, 10.25);
const _viewport = Size(200, 160);

class _CursorModel extends ChangeNotifier implements CursorModel {
  _CursorModel(this.image);

  @override
  final ui.Image image;
  @override
  double get hotx => _hotspot.dx;
  @override
  double get hoty => _hotspot.dy;
  @override
  double get x => _remotePosition.dx;
  @override
  double get y => _remotePosition.dy;

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _ImageModel extends Fake implements ImageModel {
  _ImageModel(this.useTextureRender);

  @override
  final bool useTextureRender;
}

class _Peer extends Fake implements FfiModel {
  @override
  final pi = PeerInfo();
  @override
  bool get isPeerLinux => false;
  @override
  Rect get rect => Offset.zero & _viewport;
}

class _FFI extends Fake implements FFI {
  _FFI(bool useTexture) : imageModel = _ImageModel(useTexture);

  @override
  final ImageModel imageModel;
  @override
  final ffiModel = _Peer();
}

class _CanvasModel extends ChangeNotifier implements CanvasModel {
  _CanvasModel(String style, this.scale, bool useTexture)
      : _ffi = _FFI(useTexture),
        viewStyle = ViewStyle(
          style: style,
          width: _viewport.width,
          height: _viewport.height,
          displayWidth: _viewport.width.toInt(),
          displayHeight: _viewport.height.toInt(),
        );

  final FFI _ffi;
  @override
  WeakReference<FFI> get parent => WeakReference(_ffi);
  @override
  final imageOverflow = false.obs;
  @override
  final ViewStyle viewStyle;
  @override
  double get x => _canvasOffset.dx;
  @override
  double get y => _canvasOffset.dy;
  @override
  final double scale;
  @override
  ScrollStyle get scrollStyle => ScrollStyle.scrollauto;
  @override
  Size get size => _viewport;

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

class _ScrollbarCanvasModel extends _CanvasModel {
  _ScrollbarCanvasModel(String style) : super(style, 2, true) {
    imageOverflow.value = true;
  }

  @override
  ScrollStyle get scrollStyle => ScrollStyle.scrollbar;
  @override
  double get scrollX => 0.1;
  @override
  double get scrollY => 0.2;
}

class _Canvas extends Fake implements Canvas {
  double factor = 1;
  Offset? position;

  @override
  void scale(double sx, [double? sy]) => factor *= sx;

  @override
  void drawImage(ui.Image image, Offset offset, Paint paint) {
    position = offset * factor;
  }
}

Future<ImagePainter> _paintCursor(WidgetTester tester, CanvasModel canvas,
    {double dpr = 2, bool zoom = true, (int, int) source = (48, 64)}) async {
  final image = (await tester
      .runAsync(() => createTestImage(width: source.$1, height: source.$2)))!;
  addTearDown(image.dispose);
  await tester.pumpWidget(MediaQuery(
    data: MediaQueryData(devicePixelRatio: dpr),
    child: MultiProvider(
      providers: [
        ChangeNotifierProvider<CursorModel>(create: (_) => _CursorModel(image)),
        ChangeNotifierProvider<CanvasModel>(create: (_) => canvas),
      ],
      child: CursorPaint(id: 'cursor-test', zoomCursor: zoom.obs),
    ),
  ));
  final painter = tester.widget<CustomPaint>(find.byType(CustomPaint)).painter!
      as ImagePainter;
  expect(painter.image, same(image));
  return painter;
}

void main() {
  final minimumScale = Platform.isWindows ? 1 / 3 : 2 / 3;
  for (final (style, zoom, dpr, source, canvasScale, scale, texture) in [
    (kRemoteViewStyleAdaptive, false, 2.0, (48, 64), 0.375, 0.375, true),
    (kRemoteViewStyleOriginal, false, 2.0, (48, 64), 0.5, 0.5, true),
    (kRemoteViewStyleCustom, false, 2.0, (48, 64), 0.25, 0.25, false),
    (kRemoteViewStyleCustom, true, 2.0, (48, 64), 2.0, 2.0, false),
    (kRemoteViewStyleAdaptive, false, 2.25, (48, 48), 0.375, 0.375, false),
    (kRemoteViewStyleAdaptive, true, 2.0, (9, 18), 0.1, minimumScale, true),
  ]) {
    testWidgets(
        '$style zoom=$zoom dpr=$dpr source=$source texture=$texture keeps remote geometry',
        (tester) async {
      final painter = await _paintCursor(
          tester, _CanvasModel(style, canvasScale, texture),
          dpr: dpr, zoom: zoom, source: source);
      expect(painter.scale, scale);
      var imageOrigin = _canvasOffset;
      if (!texture) {
        final background = _Canvas();
        ImagePainter(
          image: painter.image,
          x: _canvasOffset.dx / canvasScale,
          y: _canvasOffset.dy / canvasScale,
          scale: canvasScale,
        ).paint(background, _viewport);
        imageOrigin = background.position!;
      }
      final target = _remotePosition * canvasScale + imageOrigin;
      expect((Offset(painter.x, painter.y) + _hotspot) * scale, target);
      final canvas = _Canvas();
      painter.paint(canvas, _viewport);
      final position = canvas.position! + _hotspot * canvas.factor;
      expect(position.dx, closeTo(target.dx, 1e-9));
      expect(position.dy, closeTo(target.dy, 1e-9));
    });
  }
  for (final style in [kRemoteViewStyleOriginal, kRemoteViewStyleCustom]) {
    testWidgets('$style painted cursor follows scrollbar position',
        (tester) async {
      final painter = await _paintCursor(tester, _ScrollbarCanvasModel(style));
      final target = _remotePosition * 2 -
          Offset(_viewport.width * 2 * 0.1, _viewport.height * 2 * 0.2);
      expect((Offset(painter.x, painter.y) + _hotspot) * painter.scale, target);
    });
  }
}
