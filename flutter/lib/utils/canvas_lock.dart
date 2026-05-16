const String kLockCanvasOptionKey = 'lock-canvas';
const String _kCanvasLockEnabledValue = 'Y';
const String _kCanvasLockDisabledValue = 'N';

final Map<String, bool> _canvasLockCache = {};
final Map<String, int> _canvasLockRevisions = {};

bool isCanvasLockEnabled(String? value) => value == _kCanvasLockEnabledValue;

String canvasLockValue(bool enabled) =>
    enabled ? _kCanvasLockEnabledValue : _kCanvasLockDisabledValue;

bool cachedCanvasLockValue(String sessionKey) =>
    _canvasLockCache[sessionKey] ?? false;

bool hasCachedCanvasLockValue(String sessionKey) =>
    _canvasLockCache.containsKey(sessionKey);

int canvasLockRevision(String sessionKey) =>
    _canvasLockRevisions[sessionKey] ?? 0;

void setCachedCanvasLockValue(String sessionKey, bool enabled) {
  _canvasLockCache[sessionKey] = enabled;
  _canvasLockRevisions[sessionKey] = canvasLockRevision(sessionKey) + 1;
}

void syncCachedCanvasLockValue(String sessionKey, bool enabled) {
  _canvasLockCache[sessionKey] = enabled;
}

void setInitialCachedCanvasLockValue(
    String sessionKey, bool enabled, int expectedRevision) {
  if (canvasLockRevision(sessionKey) != expectedRevision ||
      _canvasLockCache.containsKey(sessionKey)) {
    return;
  }
  _canvasLockCache[sessionKey] = enabled;
}

void clearCachedCanvasLockValue(String sessionKey) {
  _canvasLockCache.remove(sessionKey);
  _canvasLockRevisions.remove(sessionKey);
}
