Future<void> observeUpdateSubmission({
  required Future<void> Function() submit,
  required void Function(Object error) onFailure,
}) async {
  try {
    await submit();
  } catch (error) {
    onFailure(error);
  }
}
