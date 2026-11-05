#include <windows.h>
#include <winsvc.h>

#include <cstdio>
#include <cwchar>
#include <string>

std::wstring StopEventName() {
  return L"Local\\RustDesk-native-stop-fixture-" +
         std::to_wstring(GetCurrentProcessId());
}

void CloseFixtureEvent(HANDLE event) {
  if (!CloseHandle(event)) {
    std::fwprintf(stderr, L"Could not close fixture event: %lu\n", GetLastError());
  }
}

void WINAPI OnServiceControl(DWORD control) {
  if (control != SERVICE_CONTROL_STOP) {
    return;
  }
  const std::wstring name = StopEventName();
  const HANDLE event = OpenEventW(EVENT_MODIFY_STATE, FALSE, name.c_str());
  if (!event) {
    std::fwprintf(stderr, L"Could not open fixture stop event: %lu\n", GetLastError());
    return;
  }
  if (!SetEvent(event)) {
    std::fwprintf(stderr, L"Could not signal fixture stop event: %lu\n", GetLastError());
  }
  CloseFixtureEvent(event);
}

void WINAPI ServiceMain(DWORD count, wchar_t** arguments) {
  if (!count) {
    std::fwprintf(stderr, L"The fixture service name is missing.\n");
    return;
  }
  const std::wstring name = StopEventName();
  const HANDLE event = CreateEventW(nullptr, TRUE, FALSE, name.c_str());
  if (!event) {
    std::fwprintf(stderr, L"Could not create fixture stop event: %lu\n", GetLastError());
    return;
  }
  const SERVICE_STATUS_HANDLE handle =
      RegisterServiceCtrlHandlerW(arguments[0], OnServiceControl);
  if (!handle) {
    std::fwprintf(stderr, L"Could not register fixture service: %lu\n", GetLastError());
    CloseFixtureEvent(event);
    return;
  }
  SERVICE_STATUS status{};
  status.dwServiceType = SERVICE_WIN32_OWN_PROCESS;
  status.dwCurrentState = SERVICE_RUNNING;
  status.dwControlsAccepted = SERVICE_ACCEPT_STOP;
  if (!SetServiceStatus(handle, &status)) {
    std::fwprintf(stderr, L"Could not report fixture service start: %lu\n", GetLastError());
  } else if (WaitForSingleObject(event, INFINITE) != WAIT_OBJECT_0) {
    std::fwprintf(stderr, L"Could not wait for fixture stop: %lu\n", GetLastError());
  }
  status.dwCurrentState = SERVICE_STOPPED;
  status.dwControlsAccepted = 0;
  if (!SetServiceStatus(handle, &status)) {
    std::fwprintf(stderr, L"Could not report fixture service stop: %lu\n", GetLastError());
  }
  CloseFixtureEvent(event);
}

int wmain(int count, wchar_t** arguments) {
  constexpr int kModeArgumentCount = 2;
  if (count != kModeArgumentCount) {
    return ERROR_BAD_ARGUMENTS;
  }
  if (std::wcscmp(arguments[1], L"--idle") == 0) {
    Sleep(INFINITE);
    return ERROR_SUCCESS;
  }
  if (std::wcscmp(arguments[1], L"--service") != 0) {
    return ERROR_BAD_ARGUMENTS;
  }
  wchar_t ignored_name[] = L"RustDeskNativeStopFixture";
  SERVICE_TABLE_ENTRYW table[] = {{ignored_name, ServiceMain}, {nullptr, nullptr}};
  return StartServiceCtrlDispatcherW(table) ? ERROR_SUCCESS
                                         : static_cast<int>(GetLastError());
}
