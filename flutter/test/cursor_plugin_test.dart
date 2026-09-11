// These tests drive the lifecycle normally owned by MouseTracker.
// ignore_for_file: invalid_use_of_protected_member

import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/services.dart';
import 'package:flutter_hbb/models/model.dart';
import 'package:flutter_hbb/native/custom_cursor.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;

class _CursorModel implements CursorModel {
  @override
  final Set<String> cachedKeys = {};
  @override
  void addKey(String key) => cachedKeys.add(key);
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

Future<CursorData> _data() async {
  final recorder = ui.PictureRecorder();
  ui.Canvas(recorder).drawColor(const ui.Color(0xff123456), ui.BlendMode.src);
  final picture = recorder.endRecording();
  final nativeImage = await picture.toImage(20, 30);
  picture.dispose();
  addTearDown(nativeImage.dispose);
  return CursorData(
      peerId: 'plugin',
      id: 'edit',
      image: img.Image(width: 20, height: 30, numChannels: 4),
      nativeImage: nativeImage,
      scale: 1,
      data: Uint8List.fromList([1, 2]),
      hotxOrigin: 4,
      hotyOrigin: 16,
      width: 20,
      height: 30);
}

void main() {
  final binding = TestWidgetsFlutterBinding.ensureInitialized();
  final view = binding.platformDispatcher.views.single;
  final channel = Platform.isWindows
      ? SystemChannels.mouseCursor
      : const MethodChannel('flutter_custom_cursor');
  late List<Map<dynamic, dynamic>> registrations;
  setUp(() {
    registrations = [];
    binding.defaultBinaryMessenger.setMockMethodCallHandler(channel,
        (call) async {
      if (call.method.startsWith('createCustomCursor')) {
        final args = call.arguments as Map<dynamic, dynamic>;
        registrations.add(args);
        return args['name'];
      }
      return null;
    });
  });
  tearDown(() {
    view.resetDevicePixelRatio();
    binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, null);
  });

  test('native bridge delegates rasterization and retains the minimum policy',
      () => _checkRasterization(view, registrations));
  test('live DPR changes invalidate a cached native cursor',
      () => _checkDprChange(view, registrations));
}

Future<void> _checkRasterization(
    TestFlutterView view, List<Map<dynamic, dynamic>> registrations) async {
  view.devicePixelRatio = 2;
  final data = await _data();
  final originalBytes = data.data;
  final cursor = _CursorModel();
  final session = buildCursorOfCache(cursor, 0.5, data).createSession(1);
  await session.activate();
  session.dispose();
  final args = registrations.single;
  final pixelsPerUnit = Platform.isWindows ? 1 : 2;
  expect(data.scale, 0.6);
  expect(identical(data.data, originalBytes), isTrue);
  expect(args['height'], 18 * pixelsPerUnit);
  expect(args['width'], (Platform.isLinux ? 18 : 12) * pixelsPerUnit);
  expect(args['imagePixelRatio'], 2.0);
  expect(args['hotX'], Platform.isMacOS ? 4.8 : 2 * pixelsPerUnit);
  expect(args['hotY'], Platform.isMacOS ? 19.2 : 10 * pixelsPerUnit);
  await deleteCustomCursor(args['name'] as String);
}

Future<void> _checkDprChange(
    TestFlutterView view, List<Map<dynamic, dynamic>> registrations) async {
  final data = await _data();
  final cursor = _CursorModel();
  for (final dpr in [2.0, 1.0]) {
    view.devicePixelRatio = dpr;
    final session = buildCursorOfCache(cursor, 1, data).createSession(1);
    await session.activate();
    session.dispose();
  }
  expect(registrations, hasLength(2));
  expect(registrations[0]['name'], isNot(registrations[1]['name']));
  for (final args in registrations) {
    await deleteCustomCursor(args['name'] as String);
  }
}
