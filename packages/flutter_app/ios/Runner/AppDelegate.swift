import Flutter
import UIKit
import UserNotifications
import CryptoKit

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    if let registrar = engineBridge.pluginRegistry.registrar(forPlugin: "PaseoPushEnvironmentPlugin") {
      PaseoPushEnvironmentPlugin.register(with: registrar)
    }
    if let registrar = engineBridge.pluginRegistry.registrar(forPlugin: "PaseoLegacyStorage") {
      let channel = FlutterMethodChannel(
        name: "sh.paseo/legacy_storage", binaryMessenger: registrar.messenger())
      channel.setMethodCallHandler { call, result in
        guard call.method == "read" else {
          result(FlutterMethodNotImplemented)
          return
        }
        DispatchQueue.global(qos: .userInitiated).async {
          do {
            let snapshot = try Self.readLegacyStorage()
            DispatchQueue.main.async { result(snapshot) }
          } catch {
            DispatchQueue.main.async {
              result(FlutterError(code: "legacy_storage_read",
                message: "Cannot read original iOS settings; source was not changed. Restart to retry.",
                details: nil))
            }
          }
        }
      }
    }
  }

  override func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    didReceive response: UNNotificationResponse,
    withCompletionHandler completionHandler: @escaping () -> Void
  ) {
    if PaseoPushEnvironmentPlugin.receive(response) {
      completionHandler()
      return
    }
    super.userNotificationCenter(center, didReceive: response, withCompletionHandler: completionHandler)
  }

  override func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    willPresent notification: UNNotification,
    withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
  ) {
    if PaseoPushEnvironmentPlugin.isDirectAPNs(notification.request.content.userInfo) {
      if #available(iOS 14.0, *) {
        completionHandler([.list])
      } else {
        completionHandler([])
      }
      return
    }
    super.userNotificationCenter(center, willPresent: notification, withCompletionHandler: completionHandler)
  }

  private static func readLegacyStorage() throws -> [String: Any] {
    let files = FileManager.default
    let support = try files.url(for: .applicationSupportDirectory, in: .userDomainMask,
      appropriateFor: nil, create: false)
    let documents = try files.url(for: .documentDirectory, in: .userDomainMask,
      appropriateFor: nil, create: false)
    guard let bundle = Bundle.main.bundleIdentifier else {
      throw CocoaError(.fileReadUnknown)
    }
    let current = support.appendingPathComponent(bundle)
      .appendingPathComponent("RCTAsyncLocalStorage_V1")
    // This is AsyncStorage's actual precedence: current Application Support
    // storage, then the old Documents location, then newest RNC/Expo storage.
    let previous = documents.appendingPathComponent("RCTAsyncLocalStorage_V1")
    let older = ["RNCAsyncLocalStorage_V1", "RCTAsyncLocalStorage"].map {
      documents.appendingPathComponent($0)
    }
    let candidates = [current, previous] + older
    var selected: URL?
    for candidate in [current, previous] {
      if files.fileExists(atPath: candidate.path) { selected = candidate; break }
    }
    if selected == nil {
      var newest = Date.distantPast
      for candidate in older {
        let manifest = candidate.appendingPathComponent("manifest.json")
        guard files.fileExists(atPath: manifest.path) else { continue }
        let attributes = try files.attributesOfItem(atPath: manifest.path)
        let modified = attributes[.modificationDate] as? Date ?? .distantPast
        if selected == nil || modified > newest { selected = candidate; newest = modified }
      }
    }
    var values = [String: String]()
    // AsyncStorage creates its directory before the first write; a directory
    // without manifest.json is an empty store, exactly as RNCAsyncStorage reads it.
    if let directory = selected,
      files.fileExists(atPath: directory.appendingPathComponent("manifest.json").path) {
      let manifest = try Data(contentsOf: directory.appendingPathComponent("manifest.json"))
      guard let entries = try JSONSerialization.jsonObject(with: manifest) as? [String: Any] else {
        throw CocoaError(.fileReadCorruptFile)
      }
      for (key, value) in entries {
        if let inline = value as? String {
          values[key] = inline
        } else if value is NSNull {
          // Values over 1024 UTF-16 units live in MD5(key) files. A missing file
          // is an absent key in RNCAsyncStorage (_getValueForKey drops it).
          let name = Insecure.MD5.hash(data: Data(key.utf8))
            .map { String(format: "%02x", $0) }.joined()
          let file = directory.appendingPathComponent(name)
          guard files.fileExists(atPath: file.path) else { continue }
          values[key] = try String(contentsOf: file, encoding: .utf8)
        } else {
          throw CocoaError(.fileReadCorruptFile)
        }
      }
      // Detect a concurrent old-app write instead of importing a torn manifest.
      guard manifest == (try Data(contentsOf: directory.appendingPathComponent("manifest.json"))) else {
        throw CocoaError(.fileReadUnknown)
      }
    }
    return ["values": values, "sources": candidates.map(\.path)]
  }
}
