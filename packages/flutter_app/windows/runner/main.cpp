#include <flutter/dart_project.h>
#include <flutter/flutter_view_controller.h>
#include <windows.h>
#include <shellapi.h>
#include <shobjidl.h>
#include <app_links/app_links_plugin_c_api.h>

#include <memory>
#include <string>

#include "flutter_window.h"
#include "utils.h"

namespace {

using ScopedHandle = std::unique_ptr<void, decltype(&::CloseHandle)>;
constexpr wchar_t kInstanceMutex[] = L"Local\\sh.paseo.sessions.desktop";
constexpr wchar_t kInstanceReady[] = L"Local\\sh.paseo.sessions.desktop.ready";

struct Activation {
  std::string project_path;
  bool app_link = false;
};

Activation ReadActivation() {
  Activation activation;
  int argc = 0;
  wchar_t** argv = CommandLineToArgvW(GetCommandLineW(), &argc);
  if (!argv) {
    return activation;
  }
  // app_links 7.2.2's SendAppLink reads exactly one native argument.
  activation.app_link = argc == 2 && _wcsnicmp(argv[1], L"paseo:", 6) == 0;
  if (!activation.app_link) {
    for (int i = 1; i < argc; ++i) {
      if (wcscmp(argv[i], L"--open-project") == 0) {
        if (++i == argc) {
          break;
        }
      } else if (argv[i][0] == L'-') {
        continue;
      }
      const DWORD attributes = GetFileAttributesW(argv[i]);
      if (attributes == INVALID_FILE_ATTRIBUTES ||
          !(attributes & FILE_ATTRIBUTE_DIRECTORY)) {
        continue;
      }
      const DWORD length = GetFullPathNameW(argv[i], 0, nullptr, nullptr);
      if (length == 0) {
        continue;
      }
      std::wstring absolute(length, L'\0');
      const DWORD copied =
          GetFullPathNameW(argv[i], length, absolute.data(), nullptr);
      if (copied > 0 && copied < length) {
        activation.project_path = Utf8FromUtf16(absolute.c_str());
        break;
      }
    }
  }
  LocalFree(argv);
  return activation;
}

bool ForwardActivation(HWND primary, const Activation& activation) {
  DWORD process_id = 0;
  GetWindowThreadProcessId(primary, &process_id);
  AllowSetForegroundWindow(process_id);
  // Use the installed exported API, not app_links' private message IDs.
  // A failed delivery must not start another engine/daemon.
  if (activation.app_link && !SendAppLink(primary)) {
    return false;
  }
  COPYDATASTRUCT data{};
  data.dwData = FlutterWindow::kActivationMessage;
  data.cbData = static_cast<DWORD>(activation.project_path.size() + 1);
  data.lpData = const_cast<char*>(activation.project_path.c_str());
  DWORD_PTR accepted = FALSE;
  return SendMessageTimeoutW(primary, WM_COPYDATA, 0,
                            reinterpret_cast<LPARAM>(&data), SMTO_ABORTIFHUNG,
                            5000, &accepted) != 0 &&
         accepted == TRUE;
}

}  // namespace

int APIENTRY wWinMain(_In_ HINSTANCE instance, _In_opt_ HINSTANCE prev,
                      _In_ wchar_t *command_line, _In_ int show_command) {
  const Activation activation = ReadActivation();
  ScopedHandle ready(CreateEventW(nullptr, TRUE, FALSE, kInstanceReady),
                     &::CloseHandle);
  ScopedHandle instance_mutex(CreateMutexW(nullptr, TRUE, kInstanceMutex),
                              &::CloseHandle);
  const DWORD mutex_status = GetLastError();
  if (!ready || !instance_mutex) {
    return EXIT_FAILURE;
  }
  if (mutex_status == ERROR_ALREADY_EXISTS) {
    // The first process may still be starting. Never create a second engine
    // merely because its hidden primary window is not ready yet.
    if (WaitForSingleObject(ready.get(), 30000) != WAIT_OBJECT_0) {
      return EXIT_FAILURE;
    }
    HWND primary = FindWindowW(Win32Window::kPrimaryWindowClass, nullptr);
    return primary && ForwardActivation(primary, activation) ? EXIT_SUCCESS
                                                            : EXIT_FAILURE;
  }
  ResetEvent(ready.get());
  // Attach to console when present (e.g., 'flutter run') or create a
  // new console when running with a debugger.
  if (!::AttachConsole(ATTACH_PARENT_PROCESS) && ::IsDebuggerPresent()) {
    CreateAndAttachConsole();
  }

  // Initialize COM, so that it is available for use in the library and/or
  // plugins.
  ::CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  SetCurrentProcessExplicitAppUserModelID(L"sh.paseo.sessions.desktop");

  flutter::DartProject project(L"data");

  // Native activation and app_links own startup routes. Do not also pass
  // them to Dart's argument parser, which would open the route twice.
  project.set_dart_entrypoint_arguments({});

  FlutterWindow window(project);
  Win32Window::Point origin(10, 10);
  Win32Window::Size size(1280, 720);
  if (!window.Create(L"Paseo Sessions", origin, size)) {
    return EXIT_FAILURE;
  }
  window.SetQuitOnClose(true);
  window.Activate(activation.project_path, true);
  SetEvent(ready.get());

  ::MSG msg;
  while (::GetMessage(&msg, nullptr, 0, 0) > 0) {
    ::TranslateMessage(&msg);
    ::DispatchMessage(&msg);
  }
  ResetEvent(ready.get());
  window.Destroy();

  ::CoUninitialize();
  return EXIT_SUCCESS;
}
