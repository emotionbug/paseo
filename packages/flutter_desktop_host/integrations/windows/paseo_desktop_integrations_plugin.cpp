#include "include/paseo_desktop_integrations/paseo_desktop_integrations_plugin_c_api.h"
#include <flutter/method_channel.h>
#include <flutter/plugin_registrar_windows.h>
#include <flutter/standard_method_codec.h>
#include <windows.h>
#include <shobjidl.h>
#include <wrl/client.h>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <algorithm>
#include <vector>

namespace {
using flutter::EncodableValue;
using flutter::EncodableMap;
using Result = flutter::MethodResult<EncodableValue>;

bool Utf16(const std::string& text, std::wstring* output) {
  if (text.empty()) { output->clear(); return true; }
  const int count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS,
    text.data(), static_cast<int>(text.size()), nullptr, 0);
  if (count == 0) return false;
  output->resize(count);
  return MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text.data(),
    static_cast<int>(text.size()), output->data(), count) == count;
}

void LaunchCommandScript(const EncodableValue* arguments, Result* result) {
  const auto* args = arguments ? std::get_if<EncodableMap>(arguments) : nullptr;
  if (!args) { result->Error("invalid_launch", "Missing launch arguments"); return; }
  const auto env_entry = args->find(EncodableValue("environment"));
  const auto count_entry = args->find(EncodableValue("argumentCount"));
  const auto* environment = env_entry == args->end() ? nullptr :
    std::get_if<EncodableMap>(&env_entry->second);
  const auto* count = count_entry == args->end() ? nullptr :
    std::get_if<int32_t>(&count_entry->second);
  if (!environment || !count || *count < 1) {
    result->Error("invalid_launch", "Missing environment or argument count"); return;
  }
  std::vector<std::wstring> entries;
  entries.reserve(environment->size());
  for (const auto& entry : *environment) {
    const auto* key = std::get_if<std::string>(&entry.first);
    const auto* value = std::get_if<std::string>(&entry.second);
    std::wstring wide;
    if (!key || !value || !Utf16(*key + "=" + *value, &wide)) {
      result->Error("invalid_environment", "Environment is not valid UTF-8"); return;
    }
    entries.push_back(std::move(wide));
  }
  std::sort(entries.begin(), entries.end(), [](const auto& a, const auto& b) {
    return _wcsicmp(a.c_str(), b.c_str()) < 0;
  });
  std::vector<wchar_t> block;
  size_t block_size = 1;
  for (const auto& entry : entries) block_size += entry.size() + 1;
  block.reserve(block_size);
  for (const auto& entry : entries) {
    block.insert(block.end(), entry.begin(), entry.end()); block.push_back(0);
  }
  block.push_back(0);
  wchar_t directory[MAX_PATH];
  const UINT length = GetSystemDirectoryW(directory, MAX_PATH);
  if (!length || length >= MAX_PATH) {
    result->Error("launch_failed", "Could not locate the Windows system directory"); return;
  }
  const std::wstring shell = std::wstring(directory, length) + LR"(\cmd.exe)";
  // CreateProcess receives cmd's exact grammar, not C-runtime argv escaping.
  // Each path is expanded exactly once from the private environment block.
  std::wstring command = L'"' + shell + LR"(" /D /V:OFF /S /C ")";
  for (int32_t i = 0; i < *count; ++i) {
    if (i) command += L" ";
    command += L'"' + std::wstring(L"%PASEO_EDITOR_ARG_") + std::to_wstring(i) + L'%' + L'"';
  }
  command += L'"';
  STARTUPINFOW startup{};
  startup.cb = sizeof(startup);
  PROCESS_INFORMATION process{};
  if (!CreateProcessW(shell.c_str(), command.data(), nullptr, nullptr, FALSE,
      DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_UNICODE_ENVIRONMENT,
      block.data(), nullptr, &startup, &process)) {
    result->Error("launch_failed", "CreateProcessW failed",
      EncodableValue(static_cast<int64_t>(GetLastError())));
    return;
  }
  CloseHandle(process.hThread);
  CloseHandle(process.hProcess);
  result->Success();
}

class PaseoDesktopIntegrationsPlugin : public flutter::Plugin {
 public:
  explicit PaseoDesktopIntegrationsPlugin(flutter::PluginRegistrarWindows* registrar)
      : registrar_(registrar), channel_(std::make_unique<flutter::MethodChannel<EncodableValue>>(
          registrar->messenger(), "paseo/desktop_integrations", &flutter::StandardMethodCodec::GetInstance())) {
    channel_->SetMethodCallHandler([this](const auto& call, auto result) {
      Handle(call, std::move(result));
    });
    window_proc_id_ = registrar_->RegisterTopLevelWindowProcDelegate(
      [this](HWND, UINT message, WPARAM wparam, LPARAM) -> std::optional<LRESULT> {
        if (lifecycle_initialized_ && message == WM_QUERYENDSESSION) {
          channel_->InvokeMethod("sessionEnding", nullptr);
          // Never veto OS shutdown. Dart teardown is best effort; Windows owns
          // the shutdown deadline and may terminate before asynchronous work ends.
          return TRUE;
        }
        if (lifecycle_initialized_ && message == WM_ENDSESSION) {
          channel_->InvokeMethod(wparam ? "sessionEnded" : "sessionEndingCancelled", nullptr);
        }
        if (lifecycle_initialized_ && message == WM_ACTIVATEAPP && wparam) {
          channel_->InvokeMethod("activated", nullptr);
        }
        return std::nullopt;
      });
  }
  ~PaseoDesktopIntegrationsPlugin() override {
    registrar_->UnregisterTopLevelWindowProcDelegate(window_proc_id_);
    channel_->SetMethodCallHandler(nullptr);
  }

 private:
  static void Error(Result* result, const char* operation, HRESULT hr) {
    result->Error(operation, "Windows native operation failed", EncodableValue(static_cast<int64_t>(hr)));
  }

  // Generate a small opaque badge icon. The taskbar copies it before it is freed.
  static HICON BadgeIcon(int64_t count) {
    constexpr int size = 32;
    BITMAPINFO bitmap_info{};
    bitmap_info.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    bitmap_info.bmiHeader.biWidth = size;
    bitmap_info.bmiHeader.biHeight = -size;
    bitmap_info.bmiHeader.biPlanes = 1;
    bitmap_info.bmiHeader.biBitCount = 32;
    bitmap_info.bmiHeader.biCompression = BI_RGB;
    void* pixels = nullptr;
    HDC dc = CreateCompatibleDC(nullptr);
    if (!dc) return nullptr;
    HBITMAP bitmap = CreateDIBSection(dc, &bitmap_info, DIB_RGB_COLORS, &pixels, nullptr, 0);
    HBITMAP mask = CreateBitmap(size, size, 1, 1, nullptr);
    if (!bitmap || !mask) {
      if (bitmap) DeleteObject(bitmap);
      if (mask) DeleteObject(mask);
      DeleteDC(dc);
      return nullptr;
    }
    HGDIOBJ old_bitmap = SelectObject(dc, bitmap);
    RECT rect{0, 0, size, size};
    HBRUSH brush = CreateSolidBrush(RGB(190, 35, 50));
    FillRect(dc, &rect, brush);
    DeleteObject(brush);
    HFONT font = CreateFontW(-20, 0, 0, 0, FW_BOLD, FALSE, FALSE, FALSE,
      DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
      ANTIALIASED_QUALITY, DEFAULT_PITCH, L"Segoe UI");
    HGDIOBJ old_font = font ? SelectObject(dc, font) : nullptr;
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, RGB(255, 255, 255));
    std::wstring text = count > 99 ? L"99+" : std::to_wstring(count);
    DrawTextW(dc, text.c_str(), static_cast<int>(text.size()), &rect,
      DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    auto* rgba = static_cast<uint32_t*>(pixels);
    for (int i = 0; i < size * size; ++i) rgba[i] |= 0xff000000;
    if (old_font) SelectObject(dc, old_font);
    if (font) DeleteObject(font);
    SelectObject(dc, old_bitmap);
    DeleteDC(dc);
    ICONINFO icon_info{};
    icon_info.fIcon = TRUE;
    icon_info.hbmColor = bitmap;
    icon_info.hbmMask = mask;
    HICON icon = CreateIconIndirect(&icon_info);
    DeleteObject(bitmap);
    DeleteObject(mask);
    return icon;
  }

  void Handle(const flutter::MethodCall<EncodableValue>& call, std::unique_ptr<Result> result) {
    if (call.method_name() == "launchCommandScript") {
      LaunchCommandScript(call.arguments(), result.get());
      return;
    }
    if (call.method_name() == "initializeLifecycle") {
      lifecycle_initialized_ = true;
      result->Success();
    } else if (call.method_name() == "disposeLifecycle") {
      lifecycle_initialized_ = false;
      result->Success();
    } else if (call.method_name() == "idleMilliseconds") {
      LASTINPUTINFO info{sizeof(LASTINPUTINFO), 0};
      if (!GetLastInputInfo(&info)) {
        Error(result.get(), "idle_failed", HRESULT_FROM_WIN32(GetLastError()));
        return;
      }
      const DWORD elapsed = GetTickCount() - info.dwTime;
      result->Success(EncodableValue(static_cast<int64_t>(elapsed)));
    } else if (call.method_name() == "setBadgeCount") {
      const auto* args = call.arguments() ? std::get_if<EncodableMap>(call.arguments()) : nullptr;
      int64_t count = -1;
      if (args) {
        auto entry = args->find(EncodableValue("count"));
        if (entry != args->end()) {
          if (const auto* n = std::get_if<int32_t>(&entry->second)) count = *n;
          if (const auto* n = std::get_if<int64_t>(&entry->second)) count = *n;
        }
      }
      if (count < 0) {
        result->Error("invalid_count", "count must be a nonnegative integer");
        return;
      }
      auto* view = registrar_->GetView();
      HWND hwnd = view ? GetAncestor(view->GetNativeWindow(), GA_ROOT) : nullptr;
      if (!hwnd) {
        result->Error("window_unavailable", "The Flutter top-level window is unavailable");
        return;
      }
      // Flutter initializes COM on its platform thread. The taskbar API applies
      // to this exact top-level window, including unpackaged desktop applications.
      Microsoft::WRL::ComPtr<ITaskbarList3> taskbar;
      HRESULT hr = CoCreateInstance(CLSID_TaskbarList, nullptr, CLSCTX_INPROC_SERVER,
        IID_PPV_ARGS(&taskbar));
      if (SUCCEEDED(hr)) hr = taskbar->HrInit();
      if (FAILED(hr)) { Error(result.get(), "taskbar_unavailable", hr); return; }
      HICON icon = count == 0 ? nullptr : BadgeIcon(count);
      if (count != 0 && !icon) {
        Error(result.get(), "badge_icon_failed", HRESULT_FROM_WIN32(GetLastError()));
        return;
      }
      const std::wstring description = std::to_wstring(count) + L" unread notifications";
      hr = taskbar->SetOverlayIcon(hwnd, icon, description.c_str());
      if (icon) DestroyIcon(icon);
      if (FAILED(hr)) { Error(result.get(), "badge_failed", hr); return; }
      result->Success();
    } else {
      result->NotImplemented();
    }
  }
  flutter::PluginRegistrarWindows* registrar_;
  std::unique_ptr<flutter::MethodChannel<EncodableValue>> channel_;
  int window_proc_id_ = -1;
  bool lifecycle_initialized_ = false;
};
}

void PaseoDesktopIntegrationsPluginCApiRegisterWithRegistrar(FlutterDesktopPluginRegistrarRef registrar) {
  auto* windows_registrar = flutter::PluginRegistrarManager::GetInstance()
    ->GetRegistrar<flutter::PluginRegistrarWindows>(registrar);
  windows_registrar->AddPlugin(std::make_unique<PaseoDesktopIntegrationsPlugin>(windows_registrar));
}
