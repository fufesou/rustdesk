import 'dart:async';
import 'dart:math';
import 'package:flutter/material.dart';
import 'package:flutter_hbb/models/input_model.dart';
import 'package:flutter_hbb/models/model.dart';
import 'package:flutter_hbb/utils/image.dart';

const int _kDotCount = 60;
const double _kDotAngle = 2 * pi / _kDotCount;
final Color _kDefaultColor = Colors.grey.withOpacity(0.7);
final Color _kTapDownColor = Colors.blue.withOpacity(0.7);
final double _baseMouseWidth = 112.0;
final double _baseMouseHeight = 138.0;
// Tolerance used for floating-point position comparisons to avoid precision errors.
final double _positionEpsilon = 0.000001;

class FloatingMouse extends StatefulWidget {
  final FFI ffi;
  const FloatingMouse({
    super.key,
    required this.ffi,
  });

  @override
  State<FloatingMouse> createState() => _FloatingMouseState();
}

class _CanvasScrollState {
  static const double speedPressed = 3.0;
  final InputModel inputModel;
  final CanvasModel canvasModel;
  final double step = 3.0;
  final int intervalMills = 30;
  Timer? timer;
  double _dx = 0;
  double _dy = 0;
  double _speed = 1.0;
  Rect _displayRect = Rect.zero;
  Offset _mouseGlobalPosition = Offset.zero;

  _CanvasScrollState({required this.inputModel, required this.canvasModel});

  set scrollX(double speed) {
    _dx = step;
    setSpeed(speed);
  }

  set scrollY(double speed) {
    _dy = step;
    setSpeed(speed);
  }

  void tryCancel() {
    _dx = 0;
    _dy = 0;
    if (timer == null) return;
    timer?.cancel();
    timer = null;
  }

  void setPressedSpeed() {
    setSpeed(_speed > 0
        ? _CanvasScrollState.speedPressed
        : -_CanvasScrollState.speedPressed);
  }

  void setReleasedSpeed() {
    setSpeed(_speed > 0 ? 1.0 : -1.0);
  }

  void setSpeed(double newSpeed) {
    _speed = newSpeed;
    if (_speed > 0) {
      _speed = _speed.clamp(0.1, 10.0);
    } else {
      _speed = _speed.clamp(-10.0, -0.1);
    }
    if (_dx != 0) {
      _dx = step * _speed;
    } else if (_dy != 0) {
      _dy = step * _speed;
    }
  }

  void tryStart(Rect displayRect, Offset mouseGlobalPosition) {
    _displayRect = displayRect;
    _mouseGlobalPosition = mouseGlobalPosition;
    if (timer != null) return;
    timer = Timer.periodic(Duration(milliseconds: intervalMills), (timer) {
      if (_dx == 0 && _dy == 0) {
        tryCancel();
      } else {
        if (_dx != 0) {
          canvasModel.panX(_dx);
        }
        if (_dy != 0) {
          canvasModel.panY(_dy);
        }
        final evt = inputModel.procEventToPeer(
            InputModel.getMouseEventMove(), _mouseGlobalPosition,
            moveCanvas: false);
        if (evt == null || checkCancelScrollTimer(evt)) {
          tryCancel();
        }
      }
    });
  }

  bool checkCancelScrollTimer(Map<String, dynamic> evt) {
    double s = canvasModel.scale;
    if (s <= 0) {
      // unreachable
      return true;
    }
    if (_dx != 0) {
      final evtX = evt['x'];
      if (evtX == null) {
        return true;
      } else if (evtX is String) {
        try {
          final x = double.tryParse(evtX);
          if (x != null) {
            if (_dx < 0) {
              if ((_displayRect.right - 1 - x).abs() < _positionEpsilon) {
                return true;
              } else {
                final dxDisplay = _dx / s;
                if ((x + dxDisplay) > (_displayRect.right - 1)) {
                  _dx = (_displayRect.right - 1 - x) * s;
                }
              }
            } else {
              if ((x - _displayRect.left).abs() < _positionEpsilon) {
                return true;
              } else {
                final dxDisplay = _dx / s;
                if ((x + dxDisplay) < _displayRect.left) {
                  _dx = (_displayRect.left - x) * s;
                }
              }
            }
          }
        } catch (e) {
          // ignore
          return true;
        }
      } else {
        return true;
      }
    }
    if (_dy != 0) {
      final evtY = evt['y'];
      if (evtY == null) {
        return true;
      } else if (evtY is String) {
        try {
          final y = double.tryParse(evtY);
          if (y != null) {
            if (_dy < 0) {
              if ((_displayRect.bottom - 1 - y).abs() < _positionEpsilon) {
                return true;
              } else {
                final dyDisplay = _dy / s;
                if ((y + dyDisplay) > (_displayRect.bottom - 1)) {
                  _dy = (_displayRect.bottom - 1 - y) * s;
                }
              }
            } else {
              if ((y - _displayRect.top).abs() < _positionEpsilon) {
                return true;
              } else {
                final dyDisplay = _dy / s;
                if ((y + dyDisplay) < _displayRect.top) {
                  _dy = (_displayRect.top - y) * s;
                }
              }
            }
          }
        } catch (e) {
          // ignore
          return true;
        }
      } else {
        return true;
      }
    }
    return false;
  }
}

class _FloatingMouseState extends State<FloatingMouse> {
  Rect? _lastBlockedRect;
  final GlobalKey _scrollWheelKey = GlobalKey();
  final GlobalKey _mouseWidgetKey = GlobalKey();
  final GlobalKey _cursorPaintKey = GlobalKey();

  Offset _position = Offset.zero;
  double? _initMouseScale;
  double _mouseScale = 1.0;
  bool _isExpanded = true;
  bool _isScrolling = false;
  Offset? _scrollCenter;
  double _snappedPointerAngle = 0.0;
  double? _lastSnappedAngle;
  late final _CanvasScrollState _canvasScrollState;

  late InputModel _inputModel;
  late CursorModel _cursorModel;
  late CanvasModel _canvasModel;

  double get mouseWidth => _baseMouseWidth * _mouseScale;
  double get mouseHeight => _baseMouseHeight * _mouseScale;

  Offset get _expandOffset =>
      Offset(84 * _getInitMouseScale(), 12 * _getInitMouseScale());

  double _getInitMouseScale() {
    if (_initMouseScale == null) {
      final size = MediaQuery.of(context).size;
      final scaleWidth = size.width * 0.3 / _baseMouseWidth;
      final scaleHeight = size.height * 0.3 / _baseMouseHeight;
      final scale = scaleWidth < scaleHeight ? scaleWidth : scaleHeight;
      _initMouseScale = scale.clamp(0.8, 1.8);
      _mouseScale = _initMouseScale!;
    }
    return _initMouseScale!;
  }

  @override
  void initState() {
    super.initState();
    _inputModel = widget.ffi.inputModel;
    _canvasModel = widget.ffi.canvasModel;
    _cursorModel = widget.ffi.cursorModel;
    _canvasScrollState =
        _CanvasScrollState(inputModel: _inputModel, canvasModel: _canvasModel);
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _updateBlockedRect();
      final size = MediaQuery.of(context).size;
      setState(() {
        _position = Offset(
          (size.width - _baseMouseWidth * _getInitMouseScale()) / 2,
          (size.height - _baseMouseHeight * _getInitMouseScale()) / 2,
        );
      });
    });
  }

  @override
  void dispose() {
    if (_lastBlockedRect != null) {
      Future.microtask(() {
        _cursorModel.removeBlockedRect(_lastBlockedRect!);
      });
    }
    _canvasScrollState.tryCancel();
    super.dispose();
  }

  void _updateBlockedRect() {
    final context = _mouseWidgetKey.currentContext;
    if (context == null) return;
    final renderBox = context.findRenderObject() as RenderBox?;
    if (renderBox == null || !renderBox.attached) return;

    final newRect = renderBox.localToGlobal(Offset.zero) & renderBox.size;

    if (_lastBlockedRect != null) {
      _cursorModel.removeBlockedRect(_lastBlockedRect!);
    }
    _cursorModel.addBlockedRect(newRect);
    _lastBlockedRect = newRect;
  }

  void _onMoveUpdateDelta(Offset delta) {
    final context = this.context;
    final size = MediaQuery.of(context).size;
    Offset newPosition = _position + delta;
    double minX = 0;
    double minY = 0;
    double maxX = size.width - mouseWidth;
    double maxY = size.height - mouseHeight;
    newPosition = Offset(
      newPosition.dx.clamp(minX, maxX),
      newPosition.dy.clamp(minY, maxY),
    );
    setState(() {
      final isPositionChanged =
          (newPosition.dx - _position.dx).abs() > _positionEpsilon ||
              (newPosition.dy - _position.dy).abs() > _positionEpsilon;
      _position = newPosition;
      if (!_isExpanded) {
        return;
      }

      Offset getMouseGlobalPosition() {
        final RenderBox? renderBox =
            _cursorPaintKey.currentContext?.findRenderObject() as RenderBox?;
        if (renderBox != null) {
          return renderBox.localToGlobal(Offset.zero);
        } else {
          return _position;
        }
      }

      Offset? getPositionFromMouseRetEvt(Map<String, dynamic> evt) {
        if (evt.containsKey('x') && evt.containsKey('y')) {
          final x = double.tryParse(evt['x'] ?? '');
          final y = double.tryParse(evt['y'] ?? '');
          if (x != null && y != null) {
            return Offset(x, y);
          }
        }
        return null;
      }

      Offset? mouseGlobalPosition;
      Offset? positionInRemoteDisplay;
      if (isPositionChanged) {
        mouseGlobalPosition = getMouseGlobalPosition();
        final evt = _inputModel.handleMouse(
            InputModel.getMouseEventMove(), mouseGlobalPosition,
            moveCanvas: false);
        if (evt != null) {
          positionInRemoteDisplay = getPositionFromMouseRetEvt(evt);
        }
        _updateBlockedRect();
      }

      // Get the display rect
      final displayRect = widget.ffi.ffiModel.displaysRect();
      if (displayRect == null) {
        _canvasScrollState.tryCancel();
        return;
      }

      // Get the mouse global position and position in remote display
      mouseGlobalPosition ??= getMouseGlobalPosition();
      if (positionInRemoteDisplay == null) {
        final evt = _inputModel.procEventToPeer(
            InputModel.getMouseEventMove(), mouseGlobalPosition,
            moveCanvas: false);
        if (evt != null) {
          positionInRemoteDisplay = getPositionFromMouseRetEvt(evt);
        }
      }

      // Check if need to start auto canvas scroll
      // If:
      // 1. The mouse is at the edge of the screen.
      // 2. The position in remote display is not at the edge of the display.
      // Then start auto canvas scroll.
      if ((_position.dx - minX).abs() < _positionEpsilon) {
        if (positionInRemoteDisplay == null ||
            (positionInRemoteDisplay.dx - displayRect.left).abs() <
                _positionEpsilon) {
          _canvasScrollState.tryCancel();
          return;
        }
        _canvasScrollState.scrollX = 1.0 * _CanvasScrollState.speedPressed;
      } else if ((_position.dy - minY).abs() < _positionEpsilon) {
        if (positionInRemoteDisplay == null ||
            (positionInRemoteDisplay.dy - displayRect.top).abs() <
                _positionEpsilon) {
          _canvasScrollState.tryCancel();
          return;
        }
        _canvasScrollState.scrollY = 1.0 * _CanvasScrollState.speedPressed;
      } else if ((_position.dx - maxX).abs() < _positionEpsilon) {
        if (positionInRemoteDisplay == null ||
            (displayRect.right - 1 - positionInRemoteDisplay.dx).abs() <
                _positionEpsilon) {
          _canvasScrollState.tryCancel();
          return;
        }
        _canvasScrollState.scrollX = -1.0 * _CanvasScrollState.speedPressed;
      } else if ((_position.dy - maxY).abs() < _positionEpsilon) {
        if (positionInRemoteDisplay == null ||
            (displayRect.bottom - 1 - positionInRemoteDisplay.dy).abs() <
                _positionEpsilon) {
          _canvasScrollState.tryCancel();
          return;
        }
        _canvasScrollState.scrollY = -1.0 * _CanvasScrollState.speedPressed;
      } else {
        _canvasScrollState.tryCancel();
        return;
      }
      _canvasScrollState.tryStart(displayRect, mouseGlobalPosition);
    });
  }

  void _onDragHandleUpdate(DragUpdateDetails details) =>
      _onMoveUpdateDelta(details.delta);

  void _onBodyPointerMoveUpdate(PointerMoveEvent event) =>
      _onMoveUpdateDelta(event.delta);

  void _handlePointerDown(PointerDownEvent event) {
    if (_isScrolling) return;

    // Get the RenderObject of the scroll wheel key and mouse widget. If unavailable, return directly and do not enter scroll mode.
    final contextScroll = _scrollWheelKey.currentContext;
    if (contextScroll == null) return;
    final RenderBox? scrollWheelBox =
        contextScroll.findRenderObject() as RenderBox?;
    if (scrollWheelBox == null || !scrollWheelBox.attached) return;

    final Rect scrollWheelRect =
        scrollWheelBox.localToGlobal(Offset.zero) & scrollWheelBox.size;

    if (scrollWheelRect.contains(event.position)) {
      final contextMouse = _mouseWidgetKey.currentContext;
      if (contextMouse == null) return;
      final RenderBox? mouseBox = contextMouse.findRenderObject() as RenderBox?;
      if (mouseBox == null || !mouseBox.attached) return;

      // Only enter scroll mode when all RenderObjects are available.
      final Offset mouseTopLeft = mouseBox.localToGlobal(Offset.zero);
      final Size mouseSize = mouseBox.size;
      final Offset center =
          mouseTopLeft + Offset(mouseSize.width / 2, mouseSize.height / 2);

      final vector = event.position - center;
      final rawAngle = atan2(vector.dy, vector.dx);

      final closestDotIndex = (rawAngle / _kDotAngle).round();
      _lastSnappedAngle = closestDotIndex * _kDotAngle;

      setState(() {
        _isScrolling = true;
        _scrollCenter = center;
        _snappedPointerAngle = _lastSnappedAngle!;
      });
    }
  }

  void _handlePointerMove(PointerMoveEvent event) {
    if (!_isScrolling || _scrollCenter == null || _lastSnappedAngle == null) {
      return;
    }

    final touchPosition = event.position;
    final vector = touchPosition - _scrollCenter!;
    final rawCurrentAngle = atan2(vector.dy, vector.dx);

    final closestDotIndex = (rawCurrentAngle / _kDotAngle).round();
    final snappedCurrentAngle = closestDotIndex * _kDotAngle;

    if (snappedCurrentAngle == _lastSnappedAngle) return;

    double deltaAngle = snappedCurrentAngle - _lastSnappedAngle!;

    if (deltaAngle.abs() > pi) {
      deltaAngle = (deltaAngle > 0) ? deltaAngle - 2 * pi : deltaAngle + 2 * pi;
    }

    _lastSnappedAngle = snappedCurrentAngle;

    setState(() {
      _snappedPointerAngle = snappedCurrentAngle;
      _inputModel.scroll(deltaAngle > 0 ? 1 : -1);
    });
  }

  void _handlePointerUp(PointerUpEvent event) {
    if (!_isScrolling) return;
    setState(() {
      _isScrolling = false;
      _lastSnappedAngle = null;
      _scrollCenter = null;
    });
  }

  @override
  Widget build(BuildContext context) {
    return Listener(
      onPointerDown: _isExpanded ? _handlePointerDown : null,
      onPointerMove: _handlePointerMove,
      onPointerUp: _handlePointerUp,
      behavior: HitTestBehavior.translucent,
      child: Stack(
        children: [
          if (!_isScrolling)
            Positioned(
              left: _position.dx,
              top: _position.dy,
              child: _buildMouseWithHide(),
            ),
          if (_isScrolling && _scrollCenter != null)
            Positioned.fill(
              child: Builder(
                builder: (context) {
                  final RenderBox? customPaintBox =
                      context.findRenderObject() as RenderBox?;
                  if (customPaintBox == null || !customPaintBox.attached) {
                    WidgetsBinding.instance.addPostFrameCallback((_) {
                      if (mounted && _isScrolling) setState(() {});
                    });
                    return const SizedBox.expand();
                  }
                  final Offset customPaintTopLeft =
                      customPaintBox.localToGlobal(Offset.zero);
                  final Offset localCenter =
                      _scrollCenter! - customPaintTopLeft;
                  return CustomPaint(
                    painter: DottedCirclePainter(
                      center: localCenter,
                      pointerAngle: _snappedPointerAngle,
                      scale: _mouseScale,
                    ),
                  );
                },
              ),
            ),
        ],
      ),
    );
  }

  Widget _buildMouseWithHide() {
    double minMouseScale = (_getInitMouseScale() * 0.3);
    if (!_isExpanded) {
      return SizedBox(
          width: mouseWidth,
          height: mouseHeight,
          child: GestureDetector(
            onPanUpdate: _onDragHandleUpdate,
            onTap: () => setState(() {
              _mouseScale = _getInitMouseScale();
              _isExpanded = true;
              _position -= _expandOffset;
            }),
            child: MouseBody(
              scrollWheelKey: _scrollWheelKey,
              mouseWidgetKey: _mouseWidgetKey,
              inputModel: _isExpanded ? _inputModel : null,
              scale: _mouseScale,
            ),
          ));
    } else {
      return SizedBox(
        width: mouseWidth,
        height: mouseHeight,
        child: Column(
          children: [
            Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                CursorPaint(
                  key: _cursorPaintKey,
                  cursorModel: _cursorModel,
                  position: Offset(0, 0),
                ),
                const Spacer(),
                GestureDetector(
                  onTap: () {
                    setState(() {
                      _mouseScale = minMouseScale;
                      _isExpanded = false;
                      _position += _expandOffset;
                    });
                  },
                  child: Container(
                    width: 18 * _mouseScale,
                    height: 18 * _mouseScale,
                    child: Center(
                      child: Container(
                        width: 14 * _mouseScale,
                        height: 14 * _mouseScale,
                        decoration: const BoxDecoration(
                          color: Colors.grey,
                          shape: BoxShape.circle,
                        ),
                        alignment: Alignment.center,
                        child: Icon(Icons.close,
                            color: Colors.white, size: 12 * _mouseScale),
                      ),
                    ),
                  ),
                ),
              ],
            ),
            Padding(
                padding: EdgeInsets.only(left: 14 * _mouseScale),
                child: MouseBody(
                  scrollWheelKey: _scrollWheelKey,
                  mouseWidgetKey: _mouseWidgetKey,
                  onPointerMoveUpdate: _onBodyPointerMoveUpdate,
                  cancelCanvasScroll: _canvasScrollState.tryCancel,
                  setCanvasScrollPressed: _canvasScrollState.setPressedSpeed,
                  setCanvasScrollReleased: _canvasScrollState.setReleasedSpeed,
                  inputModel: _isExpanded ? _inputModel : null,
                  scale: _mouseScale,
                )),
          ],
        ),
      );
    }
  }
}

class MouseBody extends StatefulWidget {
  final GlobalKey scrollWheelKey;
  final GlobalKey mouseWidgetKey;
  final Function(PointerMoveEvent)? onPointerMoveUpdate;
  final Function()? cancelCanvasScroll;
  final Function()? setCanvasScrollPressed;
  final Function()? setCanvasScrollReleased;
  final InputModel? inputModel;
  final double scale;
  const MouseBody({
    super.key,
    required this.scrollWheelKey,
    required this.mouseWidgetKey,
    required this.scale,
    this.inputModel,
    this.onPointerMoveUpdate,
    this.cancelCanvasScroll,
    this.setCanvasScrollPressed,
    this.setCanvasScrollReleased,
  });

  @override
  State<MouseBody> createState() => _MouseBodyState();
}

class _MouseBodyState extends State<MouseBody> {
  bool _leftDown = false;
  bool _rightDown = false;
  bool _dragDown = false;

  @override
  Widget build(BuildContext context) {
    final s = widget.scale;
    return Row(
      children: [
        SizedBox(
          key: widget.mouseWidgetKey,
          width: 80 * s,
          height: 120 * s,
          child: Column(
            children: [
              SizedBox(
                height: 55 * s,
                child: Stack(
                  clipBehavior: Clip.none,
                  children: [
                    Row(
                      crossAxisAlignment: CrossAxisAlignment.end,
                      children: [
                        // Left button
                        Expanded(
                          child: Listener(
                            onPointerMove: widget.onPointerMoveUpdate,
                            onPointerDown: widget.inputModel != null
                                ? (details) => setState(() {
                                      _leftDown = true;
                                      widget.inputModel
                                          ?.tapDown(MouseButtons.left);
                                    })
                                : null,
                            onPointerUp: widget.inputModel != null
                                ? (details) => setState(() {
                                      _leftDown = false;
                                      widget.inputModel
                                          ?.tapUp(MouseButtons.left);
                                      widget.cancelCanvasScroll?.call();
                                    })
                                : null,
                            child: Container(
                              decoration: BoxDecoration(
                                color:
                                    _leftDown ? _kTapDownColor : _kDefaultColor,
                                borderRadius: BorderRadius.only(
                                    topLeft: Radius.circular(22 * s)),
                              ),
                              margin: EdgeInsets.only(right: 0.5 * s),
                            ),
                          ),
                        ),
                        SizedBox(width: 1 * s),
                        // Spacer
                        SizedBox(width: 22 * s),
                        SizedBox(width: 1 * s),
                        // Right button
                        Expanded(
                          child: Listener(
                            onPointerMove: widget.onPointerMoveUpdate,
                            onPointerDown: widget.inputModel != null
                                ? (details) => setState(() {
                                      _rightDown = true;
                                      widget.inputModel
                                          ?.tapDown(MouseButtons.right);
                                    })
                                : null,
                            onPointerUp: widget.inputModel != null
                                ? (details) => setState(() {
                                      _rightDown = false;
                                      widget.inputModel
                                          ?.tapUp(MouseButtons.right);
                                      widget.cancelCanvasScroll?.call();
                                    })
                                : null,
                            child: Container(
                              decoration: BoxDecoration(
                                color: _rightDown
                                    ? _kTapDownColor
                                    : _kDefaultColor,
                                borderRadius: BorderRadius.only(
                                    topRight: Radius.circular(22 * s)),
                              ),
                              margin: EdgeInsets.only(left: 0.5 * s),
                            ),
                          ),
                        ),
                      ],
                    ),
                    // Middle function area overflows Row bottom
                    Positioned(
                      left: (80 * s - 22 * s) / 2,
                      top: 0,
                      child: Transform.translate(
                        offset: Offset(0, -2 * s),
                        child: Container(
                          width: 22 * s,
                          height: 67 * s,
                          decoration: BoxDecoration(
                            color: Colors.grey.withOpacity(0.7),
                            borderRadius: BorderRadius.vertical(
                              top: Radius.circular(12 * s),
                              bottom: Radius.circular(16 * s),
                            ),
                          ),
                          padding: EdgeInsets.symmetric(vertical: 2 * s),
                          child: Column(
                            mainAxisAlignment: MainAxisAlignment.spaceEvenly,
                            children: [
                              Stack(
                                alignment: Alignment.center,
                                children: [
                                  Container(
                                    key: widget.scrollWheelKey,
                                    width: 14 * s,
                                    height: 28 * s,
                                    decoration: BoxDecoration(
                                      color: Colors.grey.withOpacity(0.9),
                                      borderRadius:
                                          BorderRadius.circular(7 * s),
                                    ),
                                  ),
                                  Center(
                                    child: Column(
                                      mainAxisSize: MainAxisSize.min,
                                      children: [
                                        Container(
                                          width: 6 * s,
                                          height: 2 * s,
                                          color: Colors.white60,
                                        ),
                                        SizedBox(height: 3 * s),
                                        Container(
                                          width: 8 * s,
                                          height: 2 * s,
                                          color: Colors.white60,
                                        ),
                                        SizedBox(height: 3 * s),
                                        Container(
                                          width: 6 * s,
                                          height: 2 * s,
                                          color: Colors.white60,
                                        ),
                                      ],
                                    ),
                                  ),
                                ],
                              ),
                              Listener(
                                onPointerDown: (event) => widget.inputModel
                                    ?.tapDown(MouseButtons.wheel),
                                onPointerUp: (event) {
                                  widget.inputModel?.tapUp(MouseButtons.wheel);
                                  widget.cancelCanvasScroll?.call();
                                },
                                onPointerMove: widget.onPointerMoveUpdate,
                                child: Container(
                                  width: 14 * s,
                                  height: 14 * s,
                                  decoration: BoxDecoration(
                                    color: Colors.grey.withOpacity(0.9),
                                    shape: BoxShape.circle,
                                  ),
                                  child: CustomPaint(
                                    size: Size(14 * s, 14 * s),
                                    painter: FourArrowsPainter(s),
                                  ),
                                ),
                              ),
                            ],
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
              // Thin gap separates upper and lower parts
              SizedBox(height: 1 * s),
              // Bottom part: drag area (top middle indentation)
              Expanded(
                child: Listener(
                  onPointerMove: widget.onPointerMoveUpdate,
                  onPointerDown: widget.inputModel != null
                      ? (details) {
                          setState(() {
                            _dragDown = true;
                          });
                          widget.setCanvasScrollPressed?.call();
                        }
                      : null,
                  onPointerUp: widget.inputModel != null
                      ? (details) {
                          setState(() {
                            _dragDown = false;
                          });
                          widget.setCanvasScrollReleased?.call();
                        }
                      : null,
                  behavior: HitTestBehavior.opaque,
                  child: CustomPaint(
                    painter: DragAreaTopIndentPainter(
                        color: _dragDown ? _kTapDownColor : _kDefaultColor,
                        scale: widget.scale),
                    child: Container(
                      width: 80 * s,
                      alignment: Alignment.center,
                      child: Icon(Icons.drag_handle,
                          color: Colors.black54, size: 18 * s),
                    ),
                  ),
                ),
              ),
            ],
          ),
        ),
        const Spacer()
      ],
    );
  }
}

class DottedCirclePainter extends CustomPainter {
  final Offset center;
  final double pointerAngle;
  final double scale;
  final Offset? scrollWheelCenter;

  DottedCirclePainter(
      {required this.center,
      required this.pointerAngle,
      required this.scale,
      this.scrollWheelCenter});

  @override
  void paint(Canvas canvas, Size size) {
    final radius = 48.0 * scale;
    final circlePaint = Paint()
      ..color = Colors.grey.shade400
      ..style = PaintingStyle.fill;
    final pointerPaint = Paint()
      ..color = Colors.blue
      ..style = PaintingStyle.fill;

    const dotRadius = 2.5;
    for (int i = 0; i < _kDotCount; i += 3) {
      final angle = i * _kDotAngle;
      final dotX = center.dx + radius * cos(angle);
      final dotY = center.dy + radius * sin(angle);
      canvas.drawCircle(Offset(dotX, dotY), dotRadius, circlePaint);
    }

    final pointerX = center.dx + radius * cos(pointerAngle);
    final pointerY = center.dy + radius * sin(pointerAngle);
    final pointerPosition = Offset(pointerX, pointerY);
    canvas.drawCircle(pointerPosition, 8.0, pointerPaint);
  }

  @override
  bool shouldRepaint(covariant DottedCirclePainter oldDelegate) {
    return oldDelegate.pointerAngle != pointerAngle ||
        oldDelegate.center != center ||
        oldDelegate.scrollWheelCenter != scrollWheelCenter;
  }
}

// Painter for the bottom center indentation of the drag area
class BottomIndentPainter extends CustomPainter {
  @override
  void paint(Canvas canvas, Size size) {
    final paint = Paint()
      ..color = Colors.grey.withOpacity(0.7)
      ..style = PaintingStyle.fill;
    // Draw bottom semicircle
    final center = Offset(size.width / 2, size.height);
    canvas.drawArc(
      Rect.fromCenter(center: center, width: size.width, height: size.height),
      pi,
      pi,
      false,
      paint,
    );
    // Use background color to carve a circular notch in the middle
    final clearPaint = Paint()..blendMode = BlendMode.clear;
    canvas.drawCircle(Offset(size.width / 2, size.height - 10), 10, clearPaint);
  }

  @override
  bool shouldRepaint(covariant CustomPainter oldDelegate) => false;
}

// Painter for the top center indentation of the drag area
class DragAreaTopIndentPainter extends CustomPainter {
  final double scale;
  final Color color;
  DragAreaTopIndentPainter({required this.color, required this.scale});

  @override
  void paint(Canvas canvas, Size size) {
    // Use saveLayer to make the hollow part transparent
    final paint = Paint()
      ..color = color
      ..style = PaintingStyle.fill;
    canvas.saveLayer(Offset.zero & size, Paint());
    // Draw drag area main body (rectangle + bottom rounded corners)
    final rect = Rect.fromLTWH(0, 0, size.width, size.height);
    final rrect = RRect.fromRectAndCorners(
      rect,
      bottomLeft: Radius.circular(40 * scale),
      bottomRight: Radius.circular(40 * scale),
    );
    canvas.drawRRect(rrect, paint);
    // Use BlendMode.dstOut to carve a smaller semicircular notch at the top center
    final clearPaint = Paint()..blendMode = BlendMode.dstOut;
    canvas.drawArc(
      Rect.fromCenter(
          center: Offset(size.width / 2, 0),
          width: 25 * scale,
          height: 20 * scale),
      0,
      pi,
      false,
      clearPaint,
    );
    canvas.restore();
  }

  @override
  bool shouldRepaint(covariant DragAreaTopIndentPainter oldDelegate) {
    return oldDelegate.color != color || oldDelegate.scale != scale;
  }
}

class FourArrowsPainter extends CustomPainter {
  final double scale;
  FourArrowsPainter(this.scale);

  @override
  void paint(Canvas canvas, Size size) {
    final double s = scale;
    final Paint arrowPaint = Paint()
      ..color = Colors.white60
      ..style = PaintingStyle.fill;
    final Offset center = Offset(size.width / 2, size.height / 2);
    final double arrowW = 4 * s;
    final double arrowH = 3 * s;
    final double offset = 2.5 * s;
    final Path up = Path()
      ..moveTo(center.dx, center.dy - offset - arrowH)
      ..lineTo(center.dx - arrowW / 2, center.dy - offset)
      ..lineTo(center.dx + arrowW / 2, center.dy - offset)
      ..close();
    canvas.drawPath(up, arrowPaint);
    final Path down = Path()
      ..moveTo(center.dx, center.dy + offset + arrowH)
      ..lineTo(center.dx - arrowW / 2, center.dy + offset)
      ..lineTo(center.dx + arrowW / 2, center.dy + offset)
      ..close();
    canvas.drawPath(down, arrowPaint);
    final Path left = Path()
      ..moveTo(center.dx - offset - arrowH, center.dy)
      ..lineTo(center.dx - offset, center.dy - arrowW / 2)
      ..lineTo(center.dx - offset, center.dy + arrowW / 2)
      ..close();
    canvas.drawPath(left, arrowPaint);
    final Path right = Path()
      ..moveTo(center.dx + offset + arrowH, center.dy)
      ..lineTo(center.dx + offset, center.dy - arrowW / 2)
      ..lineTo(center.dx + offset, center.dy + arrowW / 2)
      ..close();
    canvas.drawPath(right, arrowPaint);
  }

  @override
  bool shouldRepaint(covariant CustomPainter oldDelegate) => false;
}

class CursorPaint extends StatelessWidget {
  final CursorModel cursorModel;
  final Offset position;
  CursorPaint({super.key, required this.cursorModel, required this.position});

  @override
  Widget build(BuildContext context) {
    double hotx = cursorModel.hotx;
    double hoty = cursorModel.hoty;
    var image = cursorModel.image;
    if (image == null) {
      if (preDefaultCursor.image != null) {
        image = preDefaultCursor.image;
        hotx = preDefaultCursor.image!.width / 2;
        hoty = preDefaultCursor.image!.height / 2;
      }
    }
    if (image == null) {
      return Offstage();
    }

    return CustomPaint(
      painter: ImagePainter(image: image, x: -hotx, y: -hoty, scale: 1.0),
    );
  }
}
