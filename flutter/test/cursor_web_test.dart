@TestOn('browser')
library;

import 'dart:convert';
import 'dart:js' as js;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter_hbb/models/model.dart' as model;
import 'package:flutter_hbb/web/custom_cursor.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;

class _CursorModel extends Fake implements model.CursorModel {
  @override
  final Set<String> cachedKeys = {};
  @override
  void addKey(String key) => cachedKeys.add(key);
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
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
