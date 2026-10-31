#include "flutter_window.h"

#include <desktop_multi_window/desktop_multi_window_plugin.h>
#include <flutter/standard_method_codec.h>

#include <optional>

#include "flutter/generated_plugin_registrant.h"

FlutterWindow::FlutterWindow(const flutter::DartProject& project)
    : project_(project) {}

FlutterWindow::~FlutterWindow() {}

void FlutterWindow::Activate(std::string project_path, bool initial) {
  flutter::EncodableMap arguments;
  if (!project_path.empty()) {
    arguments[flutter::EncodableValue("projectPath")] =
        flutter::EncodableValue(std::move(project_path));
    if (initial) {
      arguments[flutter::EncodableValue("initial")] =
          flutter::EncodableValue(true);
    }
  }
  if (!host_ready_) {
    pending_activations_.emplace_back(std::move(arguments));
    return;
  }
  host_channel_->InvokeMethod(
      "activate", std::make_unique<flutter::EncodableValue>(std::move(arguments)));
}

bool FlutterWindow::OnCreate() {
  if (!Win32Window::OnCreate()) {
    return false;
  }

  RECT frame = GetClientArea();

  // The size here must match the window dimensions to avoid unnecessary surface
  // creation / destruction in the startup path.
  flutter_controller_ = std::make_unique<flutter::FlutterViewController>(
      frame.right - frame.left, frame.bottom - frame.top, project_);
  // Ensure that basic setup of the controller was successful.
  if (!flutter_controller_->engine() || !flutter_controller_->view()) {
    return false;
  }
  RegisterPlugins(flutter_controller_->engine());
  // desktop_multi_window 0.3.1 creates secondary engines in this process.
  // Register their plugins, but never the primary-only host event channel.
  DesktopMultiWindowSetWindowCreatedCallback([](void* controller) {
    auto* window = static_cast<flutter::FlutterViewController*>(controller);
    RegisterPlugins(window->engine());
  });
  host_channel_ =
      std::make_unique<flutter::MethodChannel<flutter::EncodableValue>>(
          flutter_controller_->engine()->messenger(),
          "paseo/desktop_host_events",
          &flutter::StandardMethodCodec::GetInstance());
  host_channel_->SetMethodCallHandler(
      [this](const auto& call, auto result) {
        if (call.method_name() == "ready") {
          host_ready_ = true;
          flutter::EncodableList activations;
          activations.swap(pending_activations_);
          result->Success(flutter::EncodableValue(std::move(activations)));
          if (pending_quit_) {
            pending_quit_ = false;
            host_channel_->InvokeMethod("quit", nullptr);
          }
        } else if (call.method_name() == "allowTermination") {
          // DesktopHost has completed its guarded teardown. Only its final
          // window_manager.destroy() may now end the primary window.
          allow_termination_ = true;
          result->Success();
        } else {
          result->NotImplemented();
        }
      });
  SetChildContent(flutter_controller_->view()->GetNativeWindow());

  // The initial engine is the hidden daemon/tray coordinator. DesktopHost
  // creates every visible Sessions window through desktop_multi_window.
  // Never show this HWND, including during single-instance activation.

  return true;
}

void FlutterWindow::OnDestroy() {
  if (host_channel_) {
    host_channel_->SetMethodCallHandler(nullptr);
    host_channel_.reset();
  }
  if (flutter_controller_) {
    flutter_controller_ = nullptr;
  }

  Win32Window::OnDestroy();
}

LRESULT
FlutterWindow::MessageHandler(HWND hwnd, UINT const message,
                              WPARAM const wparam,
                              LPARAM const lparam) noexcept {
  // Give Flutter, including plugins, an opportunity to handle window messages.
  if (flutter_controller_) {
    std::optional<LRESULT> result =
        flutter_controller_->HandleTopLevelWindowProc(hwnd, message, wparam,
                                                      lparam);
    if (result) {
      return *result;
    }
  }

  if (message == WM_COPYDATA) {
    const auto* data = reinterpret_cast<const COPYDATASTRUCT*>(lparam);
    if (data && data->dwData == kActivationMessage && data->lpData &&
        data->cbData > 0) {
      const auto* text = static_cast<const char*>(data->lpData);
      if (text[data->cbData - 1] != '\0') {
        return FALSE;
      }
      Activate(std::string(text, data->cbData - 1));
      return TRUE;
    }
  }

  // window_manager normally consumes close requests and applies the host's
  // last-window/tray policy. Never let an unhandled close bypass host teardown.
  // OS session lifecycle is owned by the integrations plugin above.
  if (message == WM_CLOSE && !allow_termination_) {
    if (host_ready_) {
      host_channel_->InvokeMethod("quit", nullptr);
    } else {
      pending_quit_ = true;
    }
    return 0;
  }
  switch (message) {
    case WM_FONTCHANGE:
      if (flutter_controller_) {
        flutter_controller_->engine()->ReloadSystemFonts();
      }
      break;
  }

  return Win32Window::MessageHandler(hwnd, message, wparam, lparam);
}
