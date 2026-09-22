import 'dart:io';

import 'package:flutter_hbb/common/widgets/dialog.dart';
import 'package:flutter_hbb/generated_bridge.dart';
import 'package:flutter_hbb/models/platform_model.dart';
import 'package:flutter_test/flutter_test.dart';

class _OfflineBinding extends Fake implements RustdeskImpl {
  bool approved = false;

  @override
  bool mainIsInstalled({dynamic hint}) => true;

  @override
  Future<String> mainGetCommon({required String key, dynamic hint}) async {
    expect(key, 'credential-auth:network-status');
    return 'offline';
  }

  @override
  Future<bool> mainCheckSuperUserPermission({dynamic hint}) async => approved;
}

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('offline Network settings require local administrator approval',
      () async {
    final binding = _OfflineBinding();
    platformFFI.setBindingForTesting(binding);

    expect(await ensureCredentialAuthorization(networkSettings: true), isFalse);
    binding.approved = true;
    expect(await ensureCredentialAuthorization(networkSettings: true), isTrue);
  }, skip: !Platform.isLinux);
}
