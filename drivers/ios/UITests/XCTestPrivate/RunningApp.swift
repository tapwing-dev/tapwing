// Foreground application discovery for Tapwing's iOS Simulator XCUITest runner.
// Ported from Maestro 2.12.1 (`Routes/XCTest/RunningApp.swift`, Apache-2.0,
// Copyright (c) 2022 mobile.dev inc., modified for Tapwing).

import Foundation
import XCTest

@MainActor
enum RunningApp {
    static let springboardBundleId = "com.apple.springboard"

    /// Returns the first bundle ID in `appIds` whose state is `.runningForeground`,
    /// or `com.apple.springboard` if none matches (or `""` if `appIds` is empty).
    static func getForegroundAppId(_ appIds: [String]) -> String {
        if appIds.isEmpty {
            return ""
        }
        return appIds.first { appId in
            XCUIApplication(bundleIdentifier: appId).state == .runningForeground
        } ?? springboardBundleId
    }

    /// Queries the accessibility server for active applications and returns the
    /// first whose `XCUIApplication.state` is `.runningForeground`.
    static func getForegroundAppInfo() -> (app: XCUIApplication, bundleId: String)? {
        let runningAppIds = TapwingActiveAppsInfo().compactMap { $0["bundleId"] as? String }
        for bundleId in runningAppIds {
            let app = XCUIApplication(bundleIdentifier: bundleId)
            if app.state == .runningForeground {
                return (app, bundleId)
            }
        }
        return nil
    }

    static func getForegroundApp() -> XCUIApplication? {
        getForegroundAppInfo()?.app
    }
}
