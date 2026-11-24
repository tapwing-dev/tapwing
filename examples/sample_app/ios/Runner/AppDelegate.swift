import CoreLocation
import Flutter
import UIKit

/// Asks for "when in use" location access and reports the outcome to Flutter.
///
/// The system shows a real SpringBoard alert for this, which only the iOS
/// native lane (XCTest driver) can tap; it gives the F5 flow something to drive.
final class LocationPermissionRequester: NSObject, CLLocationManagerDelegate {
  // Strong references: the manager and the pending reply must outlive the call
  // until the user answers the alert and the delegate fires.
  private let manager = CLLocationManager()
  private var pending: FlutterResult?

  func request(_ result: @escaping FlutterResult) {
    switch manager.authorizationStatus {
    case .authorizedWhenInUse, .authorizedAlways:
      result(true)
    case .denied, .restricted:
      result(false)
    case .notDetermined:
      if pending != nil {
        result(FlutterError(code: "busy", message: "A permission request is already in flight", details: nil))
        return
      }
      pending = result
      manager.delegate = self
      manager.requestWhenInUseAuthorization()
    @unknown default:
      result(false)
    }
  }

  func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
    // Also fires once when the delegate is set; wait for a real decision.
    guard manager.authorizationStatus != .notDetermined, let reply = pending else { return }
    pending = nil
    reply(manager.authorizationStatus == .authorizedWhenInUse || manager.authorizationStatus == .authorizedAlways)
  }
}

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  private let locationPermission = LocationPermissionRequester()

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)

    // Same channel as `MainActivity.kt` on Android.
    guard let registrar = engineBridge.pluginRegistry.registrar(forPlugin: "TapwingPermissions") else { return }
    let channel = FlutterMethodChannel(
      name: "dev.tapwing.sample_app/permissions",
      binaryMessenger: registrar.messenger()
    )
    channel.setMethodCallHandler { [weak self] call, result in
      switch call.method {
      case "requestLocation":
        self?.locationPermission.request(result)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }
}
