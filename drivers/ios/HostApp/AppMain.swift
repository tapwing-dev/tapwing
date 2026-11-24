// Minimal host app required by Xcode for an iOS UITest bundle target.
// Tapwing's runner never interacts with this app; it exists solely so
// `xcodebuild test-without-building` can host `TapwingRunnerUITests`.

import UIKit

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        let vc = UIViewController()
        vc.view.backgroundColor = .systemBackground
        window.rootViewController = vc
        window.makeKeyAndVisible()
        self.window = window
        return true
    }
}
