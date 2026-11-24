// Orientation-aware screen dimensions and coordinate mapping for XCTest.
// Ported from Maestro 2.12.1 (`Routes/Helpers/ScreenSizeHelper.swift`,
// Apache-2.0, Copyright (c) 2022 mobile.dev inc., modified for Tapwing).

import Foundation
import UIKit
import XCTest

@MainActor
enum ScreenSizeHelper {
    private static var cachedSize: (Double, Double)?
    private static var lastAppBundleId: String?
    private static var lastOrientation: UIDeviceOrientation?

    static func invalidateCache() {
        cachedSize = nil
        lastAppBundleId = nil
        lastOrientation = nil
    }

    static func physicalScreenSize() -> (Double, Double) {
        let info = RunningApp.getForegroundAppInfo()
        let app = info?.app ?? XCUIApplication(bundleIdentifier: RunningApp.springboardBundleId)
        let currentAppBundleId = info?.bundleId ?? RunningApp.springboardBundleId
        let currentOrientation = XCUIDevice.shared.orientation

        if let cached = cachedSize,
           currentAppBundleId == lastAppBundleId,
           currentOrientation == lastOrientation {
            return cached
        }

        do {
            let snapshotFrame = try app.snapshot().frame
            if snapshotFrame.width > 0 && snapshotFrame.height > 0 {
                let size = (Double(snapshotFrame.width), Double(snapshotFrame.height))
                cachedSize = size
                lastAppBundleId = currentAppBundleId
                lastOrientation = currentOrientation
                return size
            }
        } catch {
            NSLog("[TapwingRunner] Screen size snapshot failed (%@); falling back to SpringBoard frame", error.localizedDescription)
        }

        let springboard = XCUIApplication(bundleIdentifier: RunningApp.springboardBundleId)
        let fallback = springboard.frame.size
        return (Double(fallback.width), Double(fallback.height))
    }

    static func actualOrientation() -> UIDeviceOrientation {
        let orientation = XCUIDevice.shared.orientation
        // Simulator often reports `.unknown` before the first explicit rotation.
        return orientation == .unknown ? .portrait : orientation
    }

    static func logicalOrientation() -> LogicalOrientation {
        switch actualOrientation() {
        case .portrait: return .portrait
        case .portraitUpsideDown: return .portraitUpsideDown
        case .landscapeLeft: return .landscapeLeft
        case .landscapeRight: return .landscapeRight
        case .faceUp: return .faceUp
        case .faceDown: return .faceDown
        default: return .portrait
        }
    }

    /// Maps hardware `UIDeviceOrientation` to the compensating `UIInterfaceOrientation`
    /// expected by `XCSynthesizedEventRecord` (landscape values are inverted per Apple).
    static func currentInterfaceOrientation() -> UIInterfaceOrientation {
        switch actualOrientation() {
        case .landscapeLeft: return .landscapeRight
        case .landscapeRight: return .landscapeLeft
        case .portrait: return .portrait
        case .portraitUpsideDown: return .portraitUpsideDown
        default: return .portrait
        }
    }

    static func actualScreenSize() -> (width: Double, height: Double, orientation: UIDeviceOrientation) {
        let orientation = actualOrientation()
        let (width, height) = physicalScreenSize()
        let adjusted = OrientationMath.actualScreenSize(
            physicalWidth: width,
            physicalHeight: height,
            orientation: logicalOrientation()
        )
        return (adjusted.width, adjusted.height, orientation)
    }

    static func orientationAwarePoint(width: Double, height: Double, point: CGPoint) -> CGPoint {
        let mapped = OrientationMath.orientationAwarePoint(
            physicalWidth: width,
            physicalHeight: height,
            point: LogicalPoint(x: Double(point.x), y: Double(point.y)),
            orientation: logicalOrientation()
        )
        return CGPoint(x: mapped.x, y: mapped.y)
    }
}
