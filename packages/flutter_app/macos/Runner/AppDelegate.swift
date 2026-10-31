import Cocoa
import FlutterMacOS

@main
class AppDelegate: FlutterAppDelegate {
  private var hostChannel: FlutterMethodChannel?
  private var hostReady = false
  private var pendingActivations: [[String: Any]] = []
  private var terminationPending = false
  private var terminationAllowed = false

  // Only the hidden coordinator engine owns activation and process termination.
  func attachDesktopHost(to controller: FlutterViewController) {
    let channel = FlutterMethodChannel(
      name: "paseo/desktop_host_events",
      binaryMessenger: controller.engine.binaryMessenger)
    hostChannel = channel
    channel.setMethodCallHandler { [weak self] call, result in
      guard let self = self else {
        result(FlutterMethodNotImplemented)
        return
      }
      switch call.method {
      case "ready":
        self.hostReady = true
        let activations = self.pendingActivations
        self.pendingActivations.removeAll()
        result(activations)
        if self.terminationPending {
          self.requestHostQuit()
        }
      case "allowTermination":
        self.terminationAllowed = true
        result(nil)
        if self.terminationPending {
          // Finish the original Quit/logout request after Dart has cleaned up.
          DispatchQueue.main.async {
            NSApp.reply(toApplicationShouldTerminate: true)
          }
        }
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }

  private func activateHost(_ arguments: [String: Any] = [:]) {
    // DesktopHost selects a user window; the coordinator must remain hidden.
    if hostReady {
      hostChannel?.invokeMethod("activate", arguments: arguments)
    } else {
      pendingActivations.append(arguments)
    }
  }

  override func application(_ application: NSApplication, open urls: [URL]) {
    // app_links is the sole URL router. Its AppleEvent handler may have already
    // delivered these URLs; handleOpen deduplicates the forwarded callback.
    super.application(application, open: urls)
    for url in urls {
      if url.isFileURL {
        var arguments: [String: Any] = ["projectPath": url.standardizedFileURL.path]
        if !hostReady {
          arguments["initial"] = true
        }
        activateHost(arguments)
      } else if url.scheme?.lowercased() == "paseo" {
        activateHost()
      }
    }
  }

  override func applicationShouldHandleReopen(
    _ sender: NSApplication, hasVisibleWindows flag: Bool
  ) -> Bool {
    activateHost()
    return false
  }

  override func applicationShouldTerminate(
    _ sender: NSApplication
  ) -> NSApplication.TerminateReply {
    if terminationAllowed {
      return .terminateNow
    }
    if !terminationPending {
      terminationPending = true
      if hostReady {
        requestHostQuit()
      }
    }
    // Deferring instead of cancelling preserves the OS logout/shutdown request.
    return .terminateLater
  }

  private func requestHostQuit() {
    hostChannel?.invokeMethod("quit", arguments: nil) { [weak self] response in
      guard let self = self, !self.terminationAllowed else { return }
      if response as? Bool == false || response is FlutterError
        || (response as AnyObject?) === FlutterMethodNotImplemented {
        self.terminationPending = false
        NSApp.reply(toApplicationShouldTerminate: false)
      }
    }
  }

  override func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    // DesktopHost decides whether the last window closes to tray or quits.
    return false
  }

  override func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool {
    return true
  }
}
