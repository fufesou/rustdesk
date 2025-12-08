/// A small helper for accumulating fractional mouse deltas and emitting integer deltas.
///
/// Relative mouse mode uses integer deltas on the wire, but Flutter pointer deltas
/// are doubles. This accumulator preserves sub-pixel movement by carrying the
/// fractional remainder across events.
class RelativeMouseDelta {
  final int x;
  final int y;

  const RelativeMouseDelta(this.x, this.y);
}

/// Accumulates fractional mouse deltas and returns integer deltas when available.
class RelativeMouseAccumulator {
  double _fracX = 0.0;
  double _fracY = 0.0;

  /// Adds a delta and returns an integer delta when at least one axis reaches a
  /// magnitude of 1px (after truncation towards zero).
  ///
  /// If [maxDelta] is > 0, the returned integer delta is clamped to
  /// [-maxDelta, maxDelta] on each axis.
  RelativeMouseDelta? add(
    double dx,
    double dy, {
    required int maxDelta,
  }) {
    _fracX += dx;
    _fracY += dy;

    int intX = _fracX.truncate();
    int intY = _fracY.truncate();

    if (intX == 0 && intY == 0) {
      return null;
    }

    _fracX -= intX;
    _fracY -= intY;

    if (maxDelta > 0) {
      intX = intX.clamp(-maxDelta, maxDelta);
      intY = intY.clamp(-maxDelta, maxDelta);
    }

    return RelativeMouseDelta(intX, intY);
  }

  void reset() {
    _fracX = 0.0;
    _fracY = 0.0;
  }
}
