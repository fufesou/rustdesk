import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/desktop/pages/desktop_setting_page.dart';
import 'package:flutter_hbb/generated_bridge.dart';
import 'package:flutter_hbb/models/platform_model.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:get/get.dart';

class _Binding extends Fake implements RustdeskImpl {
  bool unavailable = true;
  String error = 'The host is unavailable';
  final options = <String, String>{
    kOptionDirectServer: 'Y',
    kOptionAllowAutoDisconnect: 'Y',
    kOptionEnableTrustedDevices: 'N',
  };
  String get recording => options[kOptionAllowAutoRecordIncoming] ?? '';

  @override
  String translate(
          {required String name, required String locale, dynamic hint}) =>
      name;
  @override
  String mainGetBuildinOption({required String key, dynamic hint}) => '';
  @override
  String mainGetLocalOption({required String key, dynamic hint}) => '';
  @override
  String getLocalFlutterOption({required String k, dynamic hint}) => '';
  @override
  String mainGetOptionSync({required String key, dynamic hint}) =>
      options[key] ?? '';
  @override
  Future<void> mainSetOption(
      {required String key, required String value, dynamic hint}) async {
    if (unavailable) throw Exception(error);
    options[key] = value;
  }

  @override
  bool mainIsOptionFixed({required String key, dynamic hint}) => false;
  @override
  bool isIncomingOnly({dynamic hint}) => true;
  @override
  bool isOutgoingOnly({dynamic hint}) => false;
  @override
  bool isDisableSettings({dynamic hint}) => false;
  @override
  bool isDisableAccount({dynamic hint}) => true;
  @override
  bool isDisableAb({dynamic hint}) => true;
  @override
  bool isDisableGroupPanel({dynamic hint}) => true;
  @override
  bool isCustomClient({dynamic hint}) => true;
  @override
  bool mainIsInstalled({dynamic hint}) => false;
  @override
  bool mainHasHwcodec({dynamic hint}) => false;
  @override
  bool mainHasVram({dynamic hint}) => false;
  @override
  Future<String> mainGetLangs({dynamic hint}) async => '[]';
  @override
  String mainVideoSaveDirectory({required bool root, dynamic hint}) =>
      Directory.systemTemp.path;
  @override
  Future<bool> mainSupportRemoveWallpaper({dynamic hint}) async => false;
  @override
  Future<List<String>> mainGetSoundInputs({dynamic hint}) async => [];
  @override
  Future<String> mainGetOption({required String key, dynamic hint}) async => '';
  @override
  bool mainAudioSupportLoopback({dynamic hint}) => false;
  @override
  Future<String> mainGetCommon({required String key, dynamic hint}) async =>
      'true';
  @override
  Future<String> mainHandleWaylandScreencastRestoreToken(
          {required String key, required String value, dynamic hint}) async =>
      '';
  @override
  bool mainHasValid2FaSync({dynamic hint}) => false;
  @override
  bool mainHasValidBotSync({dynamic hint}) => false;
  @override
  bool mainIsShareRdp({dynamic hint}) => false;
  @override
  Future<bool> mainIsUsingPublicServer({dynamic hint}) async => false;
  @override
  String mainSupportedPrivacyModeImpls({dynamic hint}) => '[]';
}

Future<void> _openSettings(
    WidgetTester tester, _Binding binding, SettingsTabKey tab) async {
  isTest = true;
  platformFFI.setBindingForTesting(binding);
  await initGlobalFFI();
  Get.put(false.obs, tag: 'stop-service');
  addTearDown(Get.reset);
  await tester.pumpWidget(MaterialApp(
    navigatorKey: globalKey,
    theme: MyTheme.lightTheme,
    builder: (context, child) => MediaQuery(
      // Ahem test glyphs need room within the settings page's fixed-width cards.
      data: MediaQuery.of(context)
          .copyWith(textScaler: const TextScaler.linear(0.5)),
      child: child!,
    ),
    home: DesktopSettingPage(initialTabkey: tab),
  ));
  await tester.runAsync(() async => await Future<void>.delayed(Duration.zero));
  await tester.pumpAndSettle();
}

Future<void> _retryApply(
    WidgetTester tester, _Binding binding, String key) async {
  final port = key == kOptionDirectAccessPort;
  final field = find.byWidgetPredicate((widget) =>
      widget is TextField &&
      widget.decoration?.hintText == (port ? '21118' : '10'));
  await tester.ensureVisible(field);
  await tester.pumpAndSettle();
  await tester.enterText(field, port ? '21119' : '20');
  await tester.pump();
  final row = find.ancestor(of: field, matching: find.byType(Row)).first;
  final apply = find.descendant(of: row, matching: find.byType(ElevatedButton));
  binding.unavailable = true;
  await tester.tap(apply);
  await tester.pump();
  expect(tester.takeException(), isNull);
  expect(find.textContaining(binding.error), findsOneWidget);
  expect(tester.widget<ElevatedButton>(apply).onPressed, isNotNull);
  expect(binding.options[key], isNull);
  await tester.pump(const Duration(seconds: 3));
  binding.unavailable = false;
  await tester.tap(apply);
  await tester.pump();
  expect(binding.options[key], port ? '21119' : '20');
  expect(tester.widget<ElevatedButton>(apply).onPressed, isNull);
}

void main() {
  testWidgets('recording checkbox reports a failed save and permits retry',
      (tester) async {
    final binding = _Binding();
    isTest = true;
    platformFFI.setBindingForTesting(binding);
    await initGlobalFFI();
    Get.put(false.obs, tag: 'stop-service');
    addTearDown(Get.reset);
    await tester.pumpWidget(MaterialApp(
      navigatorKey: globalKey,
      theme: MyTheme.lightTheme,
      home: DesktopSettingPage(initialTabkey: SettingsTabKey.general),
    ));
    await tester
        .runAsync(() async => await Future<void>.delayed(Duration.zero));
    await tester.pumpAndSettle();
    final label = find.text('Automatically record incoming sessions');
    await tester.ensureVisible(label);
    await tester.pumpAndSettle();
    final checkbox = find.descendant(
        of: find.ancestor(of: label, matching: find.byType(Row)).first,
        matching: find.byType(Checkbox));
    expect(tester.widget<Checkbox>(checkbox).value, isFalse);
    await tester.tap(checkbox);
    await tester.pump();
    expect(tester.takeException(), isNull);
    expect(find.textContaining('The host is unavailable'), findsOneWidget);
    expect(tester.widget<Checkbox>(checkbox).value, isFalse);
    expect(binding.recording, isEmpty);
    await tester.pump(const Duration(seconds: 3));
    binding.unavailable = false;
    await tester.tap(checkbox);
    await tester.pump();
    expect(tester.widget<Checkbox>(checkbox).value, isTrue);
    expect(binding.recording, 'Y');
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('security controls report rejection and retry without editing',
      (tester) async {
    final binding = _Binding()..error = 'Authentication Required';
    await _openSettings(tester, binding, SettingsTabKey.safety);
    for (final key in [kOptionDirectAccessPort, kOptionAutoDisconnectTimeout]) {
      await _retryApply(tester, binding, key);
    }
    final accessMode = find.byWidgetPredicate((widget) =>
        widget is DropdownButton<String> && widget.value == 'Custom');
    await tester.ensureVisible(accessMode);
    await tester.pumpAndSettle();
    binding.unavailable = true;
    await tester.tap(accessMode);
    await tester.pumpAndSettle();
    await tester.tap(find.text('Full Access').last);
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
    expect(find.textContaining(binding.error), findsOneWidget);
    expect(accessMode, findsOneWidget);
    await tester.pump(const Duration(seconds: 3));
    binding.unavailable = false;
    await tester.tap(accessMode);
    await tester.pumpAndSettle();
    await tester.tap(find.text('Full Access').last);
    await tester.pumpAndSettle();
    expect(binding.options[kOptionAccessMode], 'full');
    await tester.pumpWidget(const SizedBox());
  });

  testWidgets('network switches report rejection and allow retry',
      (tester) async {
    final binding = _Binding()..error = 'Authentication Required';
    await _openSettings(tester, binding, SettingsTabKey.network);
    for (final entry in {
      'Disable UDP': kOptionDisableUdp,
      'Use WebSocket': kOptionAllowWebSocket,
      'Allow insecure TLS fallback': kOptionAllowInsecureTLSFallback,
    }.entries) {
      final tile = find.ancestor(
          of: find.text(entry.key), matching: find.byType(ListTile));
      final toggle = find.descendant(of: tile, matching: find.byType(Switch));
      await tester.ensureVisible(toggle);
      await tester.pumpAndSettle();
      binding.unavailable = true;
      await tester.tap(toggle);
      await tester.pump();
      expect(tester.takeException(), isNull);
      expect(find.textContaining(binding.error), findsOneWidget);
      expect(tester.widget<Switch>(toggle).value, isFalse);
      await tester.pump(const Duration(seconds: 3));
      binding.unavailable = false;
      await tester.tap(toggle);
      await tester.pumpAndSettle();
      expect(binding.options[entry.value], 'Y');
      expect(tester.widget<Switch>(toggle).value, isTrue);
    }
    await tester.pumpWidget(const SizedBox());
  });
}
