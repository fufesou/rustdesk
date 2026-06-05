class UpdateCancelController {
  bool _pendingCancel = false;
  bool _cancelInFlight = false;

  bool get pendingCancel => _pendingCancel;
  bool get cancelInFlight => _cancelInFlight;

  bool beginCancel(String downloadId) {
    if (_cancelInFlight) {
      return false;
    }
    if (downloadId.isEmpty) {
      _pendingCancel = true;
      return false;
    }
    _pendingCancel = false;
    _cancelInFlight = true;
    return true;
  }

  bool onDownloadIdAssigned(String downloadId) {
    if (_pendingCancel && !_cancelInFlight && downloadId.isNotEmpty) {
      _pendingCancel = false;
      return true;
    }
    return false;
  }

  void finishCancel() {
    _cancelInFlight = false;
  }
}
