import 'package:flutter_hbb/desktop/widgets/update_cancel_controller.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('pending cancel starts after the download id is assigned', () {
    final controller = UpdateCancelController();

    expect(controller.beginCancel(''), isFalse);
    expect(controller.pendingCancel, isTrue);
    expect(controller.onDownloadIdAssigned('download-id'), isTrue);
    expect(controller.pendingCancel, isFalse);
  });

  test('concurrent cancel requests are serialized', () {
    final controller = UpdateCancelController();

    expect(controller.beginCancel('download-id'), isTrue);
    expect(controller.beginCancel('download-id'), isFalse);
    expect(controller.cancelInFlight, isTrue);

    controller.finishCancel();

    expect(controller.cancelInFlight, isFalse);
    expect(controller.beginCancel('download-id'), isTrue);
  });

  test('finalizing and finished downloads cannot be canceled', () {
    expect(
      UpdateCancelController.isFinalizingOrFinished(
          '{"finalizing":true,"finished":false}'),
      isTrue,
    );
    expect(
      UpdateCancelController.isFinalizingOrFinished(
          '{"finalizing":false,"finished":true}'),
      isTrue,
    );
    expect(
      UpdateCancelController.isFinalizingOrFinished(
          '{"finalizing":false,"finished":false}'),
      isFalse,
    );
  });
}
