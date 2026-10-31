import Cocoa
import CoreGraphics
import FlutterMacOS

public final class PaseoDesktopIntegrationsPlugin: NSObject, FlutterPlugin {
  private let channel: FlutterMethodChannel
  private var shutdownObserver: NSObjectProtocol?
  private var activationObserver: NSObjectProtocol?

  private init(channel: FlutterMethodChannel) {
    self.channel = channel
    super.init()
  }

  public static func register(with registrar: FlutterPluginRegistrar) {
    let channel = FlutterMethodChannel(name: "paseo/desktop_integrations", binaryMessenger: registrar.messenger)
    let instance = PaseoDesktopIntegrationsPlugin(channel: channel)
    registrar.addMethodCallDelegate(instance, channel: channel)
  }

  public func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
    switch call.method {
    case "initializeLifecycle":
      if shutdownObserver == nil {
        shutdownObserver = NSWorkspace.shared.notificationCenter.addObserver(
          forName: NSWorkspace.willPowerOffNotification, object: nil, queue: .main
        ) { [weak self] _ in self?.channel.invokeMethod("sessionEnded", arguments: nil) }
        activationObserver = NotificationCenter.default.addObserver(
          forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in self?.channel.invokeMethod("activated", arguments: nil) }
      }
      result(nil)
    case "disposeLifecycle":
      stopLifecycle()
      result(nil)
    case "idleMilliseconds":
      // kCGAnyInputEventType reports all input rather than a null event only.
      let allInputSeconds = CGEventSource.secondsSinceLastEventType(
        .combinedSessionState, eventType: CGEventType(rawValue: UInt32.max)!)
      guard allInputSeconds.isFinite && allInputSeconds >= 0 else {
        result(FlutterError(code: "idle_failed", message: "CoreGraphics could not read system idle time", details: nil))
        return
      }
      result(Int64(allInputSeconds * 1000))
    case "setBadgeCount":
      guard let arguments = call.arguments as? [String: Any],
            let count = arguments["count"] as? NSNumber, count.int64Value >= 0 else {
        result(FlutterError(code: "invalid_count", message: "count must be a nonnegative integer", details: nil))
        return
      }
      NSApplication.shared.dockTile.badgeLabel = count.int64Value == 0 ? nil : count.stringValue
      NSApplication.shared.dockTile.display()
      result(nil)
    default:
      result(FlutterMethodNotImplemented)
    }
  }

  private func stopLifecycle() {
    if let observer = shutdownObserver { NSWorkspace.shared.notificationCenter.removeObserver(observer) }
    if let observer = activationObserver { NotificationCenter.default.removeObserver(observer) }
    shutdownObserver = nil
    activationObserver = nil
  }

  deinit { stopLifecycle() }
}
