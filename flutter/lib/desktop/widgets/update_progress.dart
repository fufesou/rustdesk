import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/desktop/widgets/update_cancel_controller.dart';
import 'package:flutter_hbb/models/platform_model.dart';
import 'package:get/get.dart';
import 'package:url_launcher/url_launcher.dart';

const _eventKeyUpdateMe = 'update-me';

Future<void> handleUpdate(String releasePageUrl) async {
  _showVerifyingUpdate();
  final downloadUrl =
      await bind.mainGetCommon(key: 'verified-download-url-$releasePageUrl');
  if (downloadUrl.startsWith('error:')) {
    final error = downloadUrl.replaceFirst('error:', '');
    _showUpdateError(releasePageUrl, error);
    return;
  }

  SimpleWrapper<String> downloadId = SimpleWrapper('');
  SimpleWrapper<VoidCallback> onCanceled = SimpleWrapper(() {});
  SimpleWrapper<VoidCallback> onCancelUnavailable = SimpleWrapper(() {});
  final cancelController = UpdateCancelController();
  SimpleWrapper<Future<void> Function()> cancelDownload =
      SimpleWrapper(() async {});
  gFFI.dialogManager.dismissAll();
  gFFI.dialogManager.show((setState, close, context) {
    cancelDownload.value = () async {
      if (!cancelController.beginCancel(downloadId.value)) {
        return;
      }
      try {
        onCanceled.value();
        await bind.mainSetCommon(
            key: 'cancel-downloader', value: downloadId.value);
        var isCanceled = false;
        var cancelUnavailable = false;
        for (int i = 0; i < 10; i++) {
          await Future.delayed(const Duration(milliseconds: 300));
          final downloadData = await bind.mainGetCommon(
              key: 'download-data-${downloadId.value}');
          isCanceled = 'error:Downloader not found' == downloadData;
          cancelUnavailable = _isDownloadFinalizingOrFinished(downloadData);
          if (isCanceled || cancelUnavailable) {
            break;
          }
        }
        if (cancelUnavailable) {
          onCancelUnavailable.value();
          return;
        }
        if (!isCanceled) {
          _showUpdateError(
            releasePageUrl,
            'Failed to confirm downloader cancellation.',
            showRetry: false,
          );
          return;
        }
        close();
      } finally {
        cancelController.finishCancel();
      }
    };
    return CustomAlertDialog(
        title: Text(translate('Downloading {$appName}')),
        content: _UpdateProgress(
                releasePageUrl,
                downloadUrl,
                downloadId,
                onCanceled,
                onCancelUnavailable,
                cancelController,
                cancelDownload)
            .marginSymmetric(horizontal: 8)
            .paddingOnly(top: 12),
        actions: [
          dialogButton(translate('Cancel'), onPressed: () async {
            await cancelDownload.value();
          }, isOutline: true),
        ]);
  });
}

bool _isDownloadFinalizingOrFinished(String downloadData) {
  if (downloadData.startsWith('error:')) {
    return false;
  }
  try {
    final decoded = jsonDecode(downloadData);
    return decoded is Map &&
        (decoded['finalizing'] == true || decoded['finished'] == true);
  } catch (_) {
    return false;
  }
}

void _showVerifyingUpdate() {
  gFFI.dialogManager.dismissAll();
  gFFI.dialogManager.show(
    (setState, close, context) => CustomAlertDialog(
      title: Text(translate('Verifying update')),
      content: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          const SizedBox(
            width: 24,
            height: 24,
            child: CircularProgressIndicator(strokeWidth: 3),
          ),
          const SizedBox(width: 16),
          Flexible(
            child: Text(
                translate('Please wait while {$appName} verifies the update.')),
          ),
        ],
      ),
    ),
    tag: 'verifying-update',
  );
}

void _showUpdateError(String releasePageUrl, String error,
    {bool showRetry = true, bool showDownloadTip = false}) {
  debugPrint('Update error: $error');
  final dialogManager = gFFI.dialogManager;

  jumplink() {
    launchUrl(Uri.parse(releasePageUrl));
    dialogManager.dismissAll();
  }

  retry() async {
    dialogManager.dismissAll();
    await handleUpdate(releasePageUrl);
  }

  dialogManager.dismissAll();
  dialogManager.show(
    (setState, close, context) => CustomAlertDialog(
      title: null,
      content: SelectionArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            if (showDownloadTip)
              msgboxContent('custom-nocancel-nook-hasclose', 'Error',
                  'download-new-version-failed-tip'),
            if (error.trim().isNotEmpty)
              ConstrainedBox(
                constraints: const BoxConstraints(maxHeight: 160),
                child: SingleChildScrollView(
                  child: SelectableText(
                    error,
                    style: const TextStyle(fontSize: 13),
                  ),
                ),
              ),
          ],
        ),
      ),
      actions: [
        dialogButton('Download', onPressed: jumplink),
        if (showRetry) dialogButton('Retry', onPressed: retry),
        dialogButton('Close', onPressed: close),
      ],
    ),
    tag: 'custom-nocancel-nook-hasclose-Error-Error',
  );
}

class _UpdateProgress extends StatefulWidget {
  final String releasePageUrl;
  final String downloadUrl;
  final SimpleWrapper<String> downloadId;
  final SimpleWrapper<VoidCallback> onCanceled;
  final SimpleWrapper<VoidCallback> onCancelUnavailable;
  final UpdateCancelController cancelController;
  final SimpleWrapper<Future<void> Function()> cancelDownload;
  _UpdateProgress(
      this.releasePageUrl,
      this.downloadUrl,
      this.downloadId,
      this.onCanceled,
      this.onCancelUnavailable,
      this.cancelController,
      this.cancelDownload,
      {Key? key})
      : super(key: key);

  @override
  State<_UpdateProgress> createState() => _UpdateProgressState();
}

class _UpdateProgressState extends State<_UpdateProgress> {
  Timer? _timer;
  int? _totalSize;
  int _downloadedSize = 0;
  bool _finished = false;
  int _getDataFailedCount = 0;
  final String _eventKeyDownloadNewVersion = 'download-new-version';

  @override
  void initState() {
    super.initState();
    widget.onCanceled.value = () {
      cancelQueryTimer();
    };
    widget.onCancelUnavailable.value = () {
      startQueryTimer();
    };
    platformFFI.registerEventHandler(_eventKeyDownloadNewVersion,
        _eventKeyDownloadNewVersion, handleDownloadNewVersion,
        replace: true);
    bind.mainSetCommon(key: 'download-new-version', value: widget.downloadUrl);
  }

  @override
  void dispose() {
    cancelQueryTimer();
    platformFFI.unregisterEventHandler(
        _eventKeyDownloadNewVersion, _eventKeyDownloadNewVersion);
    platformFFI.unregisterEventHandler(_eventKeyUpdateMe, _eventKeyUpdateMe);
    super.dispose();
  }

  void cancelQueryTimer() {
    _timer?.cancel();
    _timer = null;
  }

  void startQueryTimer() {
    if (_timer != null) {
      return;
    }
    _timer = Timer.periodic(const Duration(milliseconds: 300), (timer) {
      _updateDownloadData();
    });
  }

  Future<void> handleDownloadNewVersion(Map<String, dynamic> evt) async {
    if (evt.containsKey('id')) {
      widget.downloadId.value = evt['id'] as String;
      startQueryTimer();
      if (widget.cancelController
          .onDownloadIdAssigned(widget.downloadId.value)) {
        await widget.cancelDownload.value();
      }
    } else {
      if (evt.containsKey('error')) {
        _onError(evt['error'] as String);
      } else {
        // unreachable
        _onError('$evt');
      }
    }
  }

  void _onError(String error) {
    cancelQueryTimer();
    if (widget.downloadId.value.isNotEmpty) {
      bind.mainSetCommon(
          key: 'remove-downloader', value: widget.downloadId.value);
    }
    _showUpdateError(widget.releasePageUrl, error, showDownloadTip: true);
  }

  void _updateDownloadData() {
    String err = '';
    String downloadData =
        bind.mainGetCommonSync(key: 'download-data-${widget.downloadId.value}');
    if (downloadData.startsWith('error:')) {
      err = downloadData.substring('error:'.length);
    } else {
      try {
        jsonDecode(downloadData).forEach((key, value) {
          if (key == 'total_size') {
            if (value != null && value is int) {
              _totalSize = value;
            }
          } else if (key == 'downloaded_size') {
            _downloadedSize = value as int;
          } else if (key == 'finished') {
            _finished = value == true;
          } else if (key == 'error') {
            if (value != null) {
              err = value.toString();
            }
          }
        });
      } catch (e) {
        _getDataFailedCount += 1;
        debugPrint(
            'Failed to get download data ${widget.downloadUrl}, error $e');
        if (_getDataFailedCount > 3) {
          err = e.toString();
        }
      }
    }
    if (err != '') {
      _onError(err);
    } else {
      if (_finished) {
        cancelQueryTimer();
        bind.mainSetCommon(
            key: 'remove-downloader', value: widget.downloadId.value);
        if (_downloadedSize == 0) {
          _onError('The download file size is 0.');
        } else if (_totalSize != null && _downloadedSize != _totalSize!) {
          _onError(
              'Download finished with a size mismatch ($_downloadedSize / $_totalSize).');
        } else {
          setState(() {});
          updateMsgBox();
        }
      } else {
        setState(() {});
      }
    }
  }

  void updateMsgBox() {
    msgBox(
      gFFI.sessionId,
      'custom-nocancel',
      '{$appName} Update',
      '{$appName}-to-update-tip',
      '',
      gFFI.dialogManager,
      onSubmit: () {
        debugPrint('Downloaded, update to new version now');
        platformFFI.registerEventHandler(_eventKeyUpdateMe, _eventKeyUpdateMe,
            (evt) async {
          platformFFI.unregisterEventHandler(
              _eventKeyUpdateMe, _eventKeyUpdateMe);
          if (evt.containsKey('error')) {
            _showUpdateError(widget.releasePageUrl, evt['error'] as String,
                showRetry: false);
          }
        }, replace: true);
        bind.mainSetCommon(key: 'update-me', value: widget.downloadUrl);
      },
      submitTimeout: 5,
    );
  }

  @override
  Widget build(BuildContext context) {
    getValue() {
      if (_totalSize == null) return 0.0;
      if (_totalSize == 0) return 1.0;
      return (_downloadedSize / _totalSize!).clamp(0.0, 1.0).toDouble();
    }

    return LinearProgressIndicator(
      value: getValue(),
      minHeight: 20,
      borderRadius: BorderRadius.circular(5),
      backgroundColor: Colors.grey[300],
      valueColor: const AlwaysStoppedAnimation<Color>(Colors.blue),
    );
  }
}
