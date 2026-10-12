import 'dart:typed_data';

import 'package:flutter_hbb/web/cursor_image.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;

void main() {
  test('DRM padding does not enlarge the CSS cursor or alter its pixels', () {
    final image = img.Image(width: 256, height: 256, numChannels: 4)
      ..setPixelRgba(0, 0, 10, 20, 30, 255)
      ..setPixelRgba(48, 48, 128, 64, 32, 96);
    final cursor = trimWebCursor(Uint8List.fromList(img.encodePng(image)),
        hotX: 23, hotY: 23);

    expect((cursor.width, cursor.height), (49, 49));
    final decoded = img.decodePng(cursor.png)!;
    expect(decoded.getPixel(0, 0).toList(), [10, 20, 30, 255]);
    expect(decoded.getPixel(48, 48).toList(), [128, 64, 32, 96]);
  });

  test('transparent pixels containing the hotspot remain in the cursor', () {
    final image = img.Image(width: 256, height: 256, numChannels: 4);
    final cursor = trimWebCursor(Uint8List.fromList(img.encodePng(image)),
        hotX: 31, hotY: 22);

    expect((cursor.width, cursor.height), (32, 23));
    expect(img.decodePng(cursor.png)!.getPixel(31, 22).a, 0);
  });

  test('a cursor without trailing padding keeps the original PNG', () {
    final image = img.Image(width: 32, height: 48, numChannels: 4)
      ..setPixelRgba(31, 47, 20, 40, 60, 255);
    final png = Uint8List.fromList(img.encodePng(image));
    final cursor = trimWebCursor(png, hotX: 5, hotY: 7);

    expect((cursor.width, cursor.height), (32, 48));
    expect(cursor.png, same(png));
  });

  test('single-axis padding and one-pixel cursors retain visible pixels', () {
    for (final size in [(8, 4, 3, 4), (4, 8, 4, 3), (1, 1, 1, 1)]) {
      final image = img.Image(width: size.$1, height: size.$2, numChannels: 4)
        ..setPixelRgba(size.$3 - 1, size.$4 - 1, 10, 20, 30, 255);
      final cursor = trimWebCursor(Uint8List.fromList(img.encodePng(image)),
          hotX: 0, hotY: 0);

      expect((cursor.width, cursor.height), (size.$3, size.$4));
      expect(
          img
              .decodePng(cursor.png)!
              .getPixel(size.$3 - 1, size.$4 - 1)
              .toList(),
          [10, 20, 30, 255]);
    }
  });

  test('an invalid PNG reports a format error', () {
    expect(() => trimWebCursor(Uint8List(8), hotX: 0, hotY: 0),
        throwsFormatException);
  });
}
