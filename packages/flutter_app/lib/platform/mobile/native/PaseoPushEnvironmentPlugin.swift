import Flutter
import Foundation
import UserNotifications

/// Reads the signing profile rather than guessing APNs sandbox from build mode.
final class PaseoPushEnvironmentPlugin: NSObject, FlutterPlugin {
    private static var channel: FlutterMethodChannel?
    private static var observing = false
    private static var pending: [[String: Any]] = []

    /// Expo sends APNs directly, so Firebase's gcm.message_id-only handlers do
    /// not report these taps. Forward from AppDelegate and scene launch options.
    static func receive(_ response: UNNotificationResponse) -> Bool {
        let info = response.notification.request.content.userInfo
        guard isDirectAPNs(info) else { return false }
        var data: [String: Any] = [:]
        if let body = info["body"] as? [String: Any] {
            data = body
        } else {
            for (key, value) in info where key as? String != "aps" {
                if let key = key as? String { data[key] = value }
            }
        }
        let tap: [String: Any] = ["id": response.notification.request.identifier, "data": data]
        if observing, let channel = channel {
            channel.invokeMethod("notificationTap", arguments: tap)
        } else {
            pending.append(tap)
        }
        return true
    }

    static func isDirectAPNs(_ info: [AnyHashable: Any]) -> Bool {
        return info["aps"] != nil && info["gcm.message_id"] == nil
    }

    static func register(with registrar: FlutterPluginRegistrar) {
        let channel = FlutterMethodChannel(name: "sh.paseo/mobile_push_environment", binaryMessenger: registrar.messenger())
        self.channel = channel
        registrar.addMethodCallDelegate(PaseoPushEnvironmentPlugin(), channel: channel)
    }

    func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
        if call.method == "startObservation" {
            Self.observing = true
            let taps = Self.pending
            Self.pending.removeAll()
            result(taps)
            return
        }
        if call.method == "stopObservation" {
            Self.observing = false
            result(nil)
            return
        }
        guard call.method == "apnsEnvironment" else {
            result(FlutterMethodNotImplemented)
            return
        }
        // App Store distribution removes embedded.mobileprovision and uses production APNs.
        guard let path = Bundle.main.path(forResource: "embedded", ofType: "mobileprovision") else {
            result("production")
            return
        }
        do {
            let profile = try Data(contentsOf: URL(fileURLWithPath: path))
            let opening = Data("<?xml".utf8)
            let closing = Data("</plist>".utf8)
            guard let start = profile.range(of: opening),
                  let end = profile.range(of: closing, in: start.lowerBound..<profile.endIndex) else {
                throw NSError(domain: "PaseoPush", code: 1, userInfo: [NSLocalizedDescriptionKey: "Cannot decode the app signing profile."])
            }
            let plist = try PropertyListSerialization.propertyList(from: profile.subdata(in: start.lowerBound..<end.upperBound), options: [], format: nil)
            guard let root = plist as? [String: Any],
                  let entitlements = root["Entitlements"] as? [String: Any],
                  let environment = entitlements["aps-environment"] as? String,
                  environment == "development" || environment == "production" else {
                throw NSError(domain: "PaseoPush", code: 2, userInfo: [NSLocalizedDescriptionKey: "Push Notifications capability is missing from the signing profile."])
            }
            result(environment)
        } catch {
            result(FlutterError(code: "apns_environment", message: error.localizedDescription, details: nil))
        }
    }
}
