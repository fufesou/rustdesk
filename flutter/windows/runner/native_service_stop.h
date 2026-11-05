#ifndef RUSTDESK_RUNNER_NATIVE_SERVICE_STOP_H_
#define RUSTDESK_RUNNER_NATIVE_SERVICE_STOP_H_

#include "service_control.h"

#include <shlobj.h>
#include <tlhelp32.h>

#include <algorithm>
#include <cstdlib>
#include <limits>
#include <new>
#include <vector>

namespace native_service_stop {

constexpr DWORD kProcessExitTimeoutMilliseconds = 30000;
constexpr DWORD kImagePathCharacters = 32768;
constexpr DWORD kDecimalBase = 10;
constexpr int kArgumentCount = 5;
constexpr int kApplicationArgument = 2;
constexpr int kBrokerArgument = 3;
constexpr int kCallerArgument = 4;

struct HandleCloser {
  void operator()(HANDLE handle) const {
    if (!CloseHandle(handle)) {
      std::fwprintf(stderr, L"Could not close process handle: %lu\n", GetLastError());
    }
  }
};
using Handle = std::unique_ptr<void, HandleCloser>;

struct StopRequest {
  std::wstring application;
  std::wstring broker_image;
  DWORD caller;
};

bool IsApplicationName(std::wstring_view name) {
  return !name.empty() && std::all_of(name.begin(), name.end(), [](wchar_t value) {
    return (value >= L'a' && value <= L'z') ||
           (value >= L'A' && value <= L'Z') ||
           (value >= L'0' && value <= L'9') || value == L'_' || value == L'-';
  });
}

bool IsBrokerImage(const std::wstring& image) {
  constexpr std::wstring_view extension = L".exe";
  if (image.size() <= extension.size()) {
    return false;
  }
  const auto stem_size = image.size() - extension.size();
  return IsApplicationName(std::wstring_view(image).substr(0, stem_size)) &&
         _wcsicmp(image.c_str() + stem_size, extension.data()) == 0;
}

std::optional<DWORD> ParseCaller(std::wstring_view text) {
  DWORD value = 0;
  for (const wchar_t character : text) {
    if (character < L'0' || character > L'9') {
      return std::nullopt;
    }
    const DWORD digit = static_cast<DWORD>(character - L'0');
    if (value > (std::numeric_limits<DWORD>::max() - digit) / kDecimalBase) {
      return std::nullopt;
    }
    value = value * kDecimalBase + digit;
  }
  return value ? std::optional<DWORD>(value) : std::nullopt;
}

DWORD RemoveStartupShortcut(const std::wstring& application) {
  PWSTR raw_directory = nullptr;
  const HRESULT result =
      SHGetKnownFolderPath(FOLDERID_ProgramData, 0, nullptr, &raw_directory);
  if (FAILED(result)) {
    return static_cast<DWORD>(result);
  }
  const std::unique_ptr<wchar_t, decltype(&CoTaskMemFree)> directory(
      raw_directory, CoTaskMemFree);
  const std::wstring path =
      std::wstring(directory.get()) +
      L"\\Microsoft\\Windows\\Start Menu\\Programs\\Startup\\" + application +
      L" Tray.lnk";
  const DWORD attributes = GetFileAttributesW(path.c_str());
  if (attributes != INVALID_FILE_ATTRIBUTES &&
      (attributes & FILE_ATTRIBUTE_READONLY) &&
      !SetFileAttributesW(path.c_str(), attributes & ~FILE_ATTRIBUTE_READONLY)) {
    return GetLastError();
  }
  if (DeleteFileW(path.c_str())) {
    return ERROR_SUCCESS;
  }
  const DWORD error = GetLastError();
  return error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND
             ? ERROR_SUCCESS
             : error;
}

DWORD ErrorUnlessExited(HANDLE process, DWORD error) {
  return WaitForSingleObject(process, 0) == WAIT_OBJECT_0 ? ERROR_SUCCESS : error;
}

DWORD TerminateMatchingProcess(DWORD pid, const std::wstring& expected_image) {
  Handle process(OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION |
                                 SYNCHRONIZE,
                             FALSE, pid));
  if (!process) {
    const DWORD error = GetLastError();
    return error == ERROR_INVALID_PARAMETER ? ERROR_SUCCESS : error;
  }
  std::vector<wchar_t> image(kImagePathCharacters);
  DWORD length = static_cast<DWORD>(image.size());
  if (!QueryFullProcessImageNameW(process.get(), 0, image.data(), &length)) {
    return ErrorUnlessExited(process.get(), GetLastError());
  }
  const std::wstring_view path(image.data(), length);
  const auto separator = path.find_last_of(L"\\/");
  const wchar_t* filename =
      image.data() + (separator == std::wstring_view::npos ? 0 : separator + 1);
  // A PID may have been reused since the snapshot; act on this pinned image only.
  if (_wcsicmp(filename, expected_image.c_str()) != 0) {
    return ERROR_SUCCESS;
  }
  if (!TerminateProcess(process.get(), ERROR_PROCESS_ABORTED)) {
    return ErrorUnlessExited(process.get(), GetLastError());
  }
  const DWORD wait = WaitForSingleObject(process.get(), kProcessExitTimeoutMilliseconds);
  if (wait == WAIT_OBJECT_0) {
    return ERROR_SUCCESS;
  }
  return wait == WAIT_FAILED ? GetLastError() : ERROR_TIMEOUT;
}

bool IsTargetProcess(const PROCESSENTRY32W& entry, const std::wstring& image,
                     DWORD caller) {
  return entry.th32ProcessID != caller && entry.th32ProcessID != GetCurrentProcessId() &&
         _wcsicmp(entry.szExeFile, image.c_str()) == 0;
}

DWORD TerminateImage(const std::wstring& image, DWORD caller) {
  const HANDLE raw_snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
  if (raw_snapshot == INVALID_HANDLE_VALUE) {
    return GetLastError();
  }
  const Handle snapshot(raw_snapshot);
  PROCESSENTRY32W entry{};
  entry.dwSize = sizeof(entry);
  if (!Process32FirstW(snapshot.get(), &entry)) {
    const DWORD error = GetLastError();
    return error == ERROR_NO_MORE_FILES ? ERROR_SUCCESS : error;
  }
  do {
    if (!IsTargetProcess(entry, image, caller)) {
      continue;
    }
    const DWORD error = TerminateMatchingProcess(entry.th32ProcessID, image);
    if (error != ERROR_SUCCESS) {
      std::fwprintf(stderr, L"Could not terminate %ls (PID %lu): %lu\n",
                    image.c_str(), entry.th32ProcessID, error);
      return error;
    }
  } while (Process32NextW(snapshot.get(), &entry));
  const DWORD error = GetLastError();
  return error == ERROR_NO_MORE_FILES ? ERROR_SUCCESS : error;
}

DWORD StopAndClean(const StopRequest& request) {
  const DWORD removed = service_control::RemoveService(request.application.c_str());
  if (removed != ERROR_SUCCESS) {
    return removed;
  }
  const DWORD shortcut = RemoveStartupShortcut(request.application);
  if (shortcut != ERROR_SUCCESS) {
    return shortcut;
  }
  const DWORD broker = TerminateImage(request.broker_image, request.caller);
  if (broker != ERROR_SUCCESS) {
    return broker;
  }
  return TerminateImage(request.application + L".exe", request.caller);
}



inline DWORD RunCommand(int count, wchar_t** arguments) {
  if (count != kArgumentCount) {
    std::fwprintf(stderr, L"Expected application name, broker image and caller PID.\n");
    return ERROR_BAD_ARGUMENTS;
  }
  try {
    const auto caller = ParseCaller(arguments[kCallerArgument]);
    const std::wstring application(arguments[kApplicationArgument]);
    const std::wstring broker(arguments[kBrokerArgument]);
    if (!caller || !IsApplicationName(application) || !IsBrokerImage(broker)) {
      return ERROR_INVALID_PARAMETER;
    }
    const DWORD result = StopAndClean({application, broker, *caller});
    if (result != ERROR_SUCCESS) {
      std::fwprintf(stderr, L"Native service stop failed: 0x%08lx\n", result);
    }
    return result;
  } catch (const std::bad_alloc&) {
    return ERROR_NOT_ENOUGH_MEMORY;
  } catch (const std::exception& error) {
    std::fprintf(stderr, "Native service stop failed: %s\n", error.what());
    return ERROR_GEN_FAILURE;
  }
}

inline std::optional<DWORD> HandleCommand(const wchar_t* command_line) {
  constexpr std::wstring_view option = L"--native-service-stop";
  const std::wstring_view arguments(command_line);
  if (arguments.compare(0, option.size(), option) != 0 ||
      (arguments.size() > option.size() && arguments[option.size()] != L' ' &&
       arguments[option.size()] != L'\t')) {
    return std::nullopt;
  }
  int count = 0;
  wchar_t** args = CommandLineToArgvW(GetCommandLineW(), &count);
  if (!args) {
    return GetLastError();
  }
  const DWORD result = RunCommand(count, args);
  LocalFree(args);
  return result;
}

}  // namespace native_service_stop

#endif  // RUSTDESK_RUNNER_NATIVE_SERVICE_STOP_H_
