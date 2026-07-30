import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/desktop/widgets/update_cancel_controller.dart';
import 'package:flutter_hbb/models/platform_model.dart';
import 'package:get/get.dart';
import 'package:url_launcher/url_launcher.dart';

final _isExtracting = false.obs;
const _downloadPollInterval = Duration(milliseconds: 300);
const _cancelPollAttempts = 10;

void handleUpdate(String releasePageUrl) {
  _isExtracting.value = false;
  String downloadUrl = releasePageUrl.replaceAll('tag', 'download');
  String version = downloadUrl.substring(downloadUrl.lastIndexOf('/') + 1);
  final String downloadFile =
      bind.mainGetCommonSync(key: 'download-file-$version');
  if (downloadFile.startsWith('error:')) {
    final error = downloadFile.replaceFirst('error:', '');
    msgBox(gFFI.sessionId, 'custom-nocancel-nook-hasclose', 'Error', error,
        releasePageUrl, gFFI.dialogManager);
    return;
  }
  downloadUrl = '$downloadUrl/$downloadFile';

  final progressKey = GlobalKey<UpdateProgressState>();
  gFFI.dialogManager.dismissAll();
  gFFI.dialogManager.show((setState, close, context) {
    return CustomAlertDialog(
        title: Obx(() => Text(translate(_isExtracting.isTrue
            ? 'Preparing for installation ...'
            : 'Downloading {$appName}'))),
        content: UpdateProgress(releasePageUrl, downloadUrl, key: progressKey)
            .marginSymmetric(horizontal: 8)
            .paddingOnly(top: 12),
        actions: [
          if (_isExtracting.isFalse)
            dialogButton(translate('Cancel'), onPressed: () async {
              await progressKey.currentState?.cancelDownload(close);
            }, isOutline: true),
        ]);
  });
}

class UpdateProgress extends StatefulWidget {
  final String releasePageUrl;
  final String downloadUrl;
  const UpdateProgress(this.releasePageUrl, this.downloadUrl, {Key? key})
      : super(key: key);

  @override
  State<UpdateProgress> createState() => UpdateProgressState();
}

class UpdateProgressState extends State<UpdateProgress> {
  final UpdateCancelController _cancelController = UpdateCancelController();
  Timer? _timer;
  String _downloadId = '';
  VoidCallback? _pendingCancelClose;
  int? _totalSize;
  int _downloadedSize = 0;
  bool _finished = false;
  int _getDataFailedCount = 0;
  final String _eventKeyDownloadNewVersion = 'download-new-version';
  final String _eventKeyExtractUpdateDmg = 'extract-update-dmg';

  @override
  void initState() {
    super.initState();
    platformFFI.registerEventHandler(_eventKeyDownloadNewVersion,
        _eventKeyDownloadNewVersion, handleDownloadNewVersion,
        replace: true);
    bind.mainSetCommon(key: 'download-new-version', value: widget.downloadUrl);
    if (isMacOS) {
      platformFFI.registerEventHandler(_eventKeyExtractUpdateDmg,
          _eventKeyExtractUpdateDmg, handleExtractUpdateDmg,
          replace: true);
    }
  }

  @override
  void dispose() {
    cancelQueryTimer();
    _pendingCancelClose = null;
    platformFFI.unregisterEventHandler(
        _eventKeyDownloadNewVersion, _eventKeyDownloadNewVersion);
    if (isMacOS) {
      platformFFI.unregisterEventHandler(
          _eventKeyExtractUpdateDmg, _eventKeyExtractUpdateDmg);
    }
    super.dispose();
  }

  void cancelQueryTimer() {
    _timer?.cancel();
    _timer = null;
  }

  void startQueryTimer() {
    cancelQueryTimer();
    _timer = Timer.periodic(_downloadPollInterval, (timer) {
      _updateDownloadData();
    });
  }

  Future<void> cancelDownload(VoidCallback close) async {
    if (!_cancelController.beginCancel(_downloadId)) {
      if (_downloadId.isEmpty) {
        _pendingCancelClose = close;
      }
      return;
    }
    _pendingCancelClose = null;
    try {
      cancelQueryTimer();
      await bind.mainSetCommon(key: 'cancel-downloader', value: _downloadId);
      for (var attempt = 0; attempt < _cancelPollAttempts; attempt++) {
        await Future.delayed(_downloadPollInterval);
        if (!mounted) {
          return;
        }
        final data =
            await bind.mainGetCommon(key: 'download-data-$_downloadId');
        if (data == 'error:Downloader not found') {
          close();
          return;
        }
        if (UpdateCancelController.isFinalizingOrFinished(data)) {
          startQueryTimer();
          return;
        }
      }
      debugPrint('Failed to confirm downloader cancellation for $_downloadId');
      startQueryTimer();
    } finally {
      _cancelController.finishCancel();
    }
  }

  Future<void> handleDownloadNewVersion(Map<String, dynamic> evt) async {
    if (evt.containsKey('id')) {
      _downloadId = evt['id'] as String;
      startQueryTimer();
      final pendingClose = _pendingCancelClose;
      if (pendingClose != null &&
          _cancelController.onDownloadIdAssigned(_downloadId)) {
        _pendingCancelClose = null;
        await cancelDownload(pendingClose);
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

  // `isExtractDmg` is true when handling extract-update-dmg event.
  // It's a rare case that the dmg file is corrupted and cannot be extracted.
  void _onError(String error, {bool isExtractDmg = false}) {
    cancelQueryTimer();

    debugPrint(
        '${isExtractDmg ? "Extract" : "Download"} new version error: $error');
    final msgBoxType = 'custom-nocancel-nook-hasclose';
    final msgBoxTitle = 'Error';
    final msgBoxText = 'download-new-version-failed-tip';
    final dialogManager = gFFI.dialogManager;

    close() {
      dialogManager.dismissAll();
    }

    jumplink() {
      launchUrl(Uri.parse(widget.releasePageUrl));
      dialogManager.dismissAll();
    }

    retry() {
      dialogManager.dismissAll();
      handleUpdate(widget.releasePageUrl);
    }

    final List<Widget> buttons = [
      dialogButton('Download', onPressed: jumplink),
      if (!isExtractDmg) dialogButton('Retry', onPressed: retry),
      dialogButton('Close', onPressed: close),
    ];
    dialogManager.dismissAll();
    dialogManager.show(
      (setState, close, context) => CustomAlertDialog(
        title: null,
        content: SelectionArea(
            child: msgboxContent(msgBoxType, msgBoxTitle, msgBoxText)),
        actions: buttons,
      ),
      tag: '$msgBoxType-$msgBoxTitle-$msgBoxTitle',
    );
  }

  void _updateDownloadData() {
    String err = '';
    String downloadData =
        bind.mainGetCommonSync(key: 'download-data-$_downloadId');
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
      if (_finished && _totalSize != null && _downloadedSize >= _totalSize!) {
        cancelQueryTimer();
        bind.mainSetCommon(key: 'remove-downloader', value: _downloadId);
        if (_totalSize == 0) {
          _onError('The download file size is 0.');
        } else {
          setState(() {});
          if (isMacOS) {
            bind.mainSetCommon(
                key: 'extract-update-dmg', value: widget.downloadUrl);
            _isExtracting.value = true;
          } else {
            updateMsgBox();
          }
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
        bind.mainSetCommon(key: 'update-me', value: widget.downloadUrl);
      },
      submitTimeout: 5,
    );
  }

  Future<void> handleExtractUpdateDmg(Map<String, dynamic> evt) async {
    _isExtracting.value = false;
    if (evt.containsKey('err') && (evt['err'] as String).isNotEmpty) {
      _onError(evt['err'] as String, isExtractDmg: true);
    } else {
      updateMsgBox();
    }
  }

  @override
  Widget build(BuildContext context) {
    getValue() => _totalSize == null
        ? 0.0
        : (_totalSize == 0 ? 1.0 : _downloadedSize / _totalSize!);
    return LinearProgressIndicator(
      value: _isExtracting.isTrue ? null : getValue(),
      minHeight: 20,
      borderRadius: BorderRadius.circular(5),
      backgroundColor: Colors.grey[300],
      valueColor: const AlwaysStoppedAnimation<Color>(Colors.blue),
    );
  }
}
