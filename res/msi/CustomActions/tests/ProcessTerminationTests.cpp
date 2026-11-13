#include <windows.h>
#include <winternl.h>
#include <cstdio>

using QueryProcess = NTSTATUS(NTAPI *)(HANDLE, PROCESSINFOCLASS, PVOID, ULONG, PULONG);
bool TerminateProcessIfNotContainsParam(QueryProcess query, HANDLE process, LPCWSTR excludeParam);

struct ChildProcess
{
    PROCESS_INFORMATION info = {};

    ~ChildProcess()
    {
        if (info.hProcess != NULL)
        {
            TerminateProcess(info.hProcess, 0);
            WaitForSingleObject(info.hProcess, 5000);
            CloseHandle(info.hProcess);
            CloseHandle(info.hThread);
        }
    }

    bool Start()
    {
        wchar_t path[MAX_PATH];
        if (GetModuleFileNameW(NULL, path, MAX_PATH) == 0)
            return false;
        STARTUPINFOW startup = {};
        startup.cb = sizeof(startup);
        return CreateProcessW(path, NULL, NULL, NULL, FALSE,
            CREATE_SUSPENDED | CREATE_NO_WINDOW, NULL, NULL, &startup, &info) != FALSE;
    }
};

int main()
{
    const auto query = reinterpret_cast<QueryProcess>(
        GetProcAddress(GetModuleHandleW(L"ntdll.dll"), "NtQueryInformationProcess"));
    ChildProcess denied;
    ChildProcess allowed;
    if (query == NULL || !denied.Start() || !allowed.Start())
        return 2;

    HANDLE restricted = OpenProcess(
        PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | SYNCHRONIZE, FALSE, denied.info.dwProcessId);
    if (restricted == NULL)
        return 2;
    const bool deniedResult = TerminateProcessIfNotContainsParam(query, restricted, L"--install");
    CloseHandle(restricted);
    if (deniedResult)
    {
        fprintf(stderr, "FAIL: denied process termination was reported as successful\n");
        return 1;
    }
    if (!TerminateProcessIfNotContainsParam(query, allowed.info.hProcess, L"--install") ||
        WaitForSingleObject(allowed.info.hProcess, 0) != WAIT_OBJECT_0)
    {
        fprintf(stderr, "FAIL: successful process cleanup did not confirm exit\n");
        return 1;
    }
    puts("PASS: denied termination fails and successful cleanup confirms exit");
    return 0;
}
