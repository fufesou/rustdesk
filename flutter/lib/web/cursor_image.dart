import 'dart:math';
import 'dart:typed_data';

import 'package:image/image.dart' as img;

({Uint8List png, int width, int height}) trimWebCursor(Uint8List png,
    {required int hotX, required int hotY}) {
  final image = img.decodePng(png);
  if (image == null) {
    throw const FormatException('Invalid PNG for Web cursor');
  }
  // DRM buffers can exceed CSS cursor limits despite containing small artwork.
  // Keep the origin and hotspot; remove only transparent right/bottom padding.
  var width = hotX.clamp(0, image.width - 1) + 1;
  var height = hotY.clamp(0, image.height - 1) + 1;
  for (final pixel in image) {
    if (pixel.a == 0) continue;
    width = max(width, pixel.x + 1);
    height = max(height, pixel.y + 1);
  }
  if (width == image.width && height == image.height) {
    return (png: png, width: width, height: height);
  }
  final cropped = img.copyCrop(image, x: 0, y: 0, width: width, height: height);
  return (
    png: Uint8List.fromList(img.encodePng(cropped)),
    width: width,
    height: height,
  );
}
