// Pure orientation-aware screen dimension and point-rotation math, shared
// between the iOS XCUITest runner and macOS host unit tests.
//
// Ported from Maestro 2.12.1 (`Routes/Helpers/ScreenSizeHelper.swift` and
// `Routes/Handlers/ViewHierarchyHandler.swift`, Apache-2.0, Copyright (c) 2022
// mobile.dev inc., modified for Tapwing).

import Foundation

/// Platform-neutral mirror of `UIDeviceOrientation` for unit-testable math.
public enum LogicalOrientation: Equatable, Sendable {
    case portrait
    case portraitUpsideDown
    case landscapeLeft
    case landscapeRight
    case faceUp
    case faceDown
}

public struct LogicalPoint: Equatable, Sendable {
    public let x: Double
    public let y: Double

    public init(x: Double, y: Double) {
        self.x = x
        self.y = y
    }
}

public enum OrientationMath {
    /// Returns the orientation-adjusted screen size in logical points.
    /// When the snapshot already reports landscape-swapped dimensions, they
    /// are preserved rather than double-swapped.
    public static func actualScreenSize(
        physicalWidth: Double,
        physicalHeight: Double,
        orientation: LogicalOrientation
    ) -> (width: Double, height: Double) {
        let isLandscape = orientation == .landscapeLeft || orientation == .landscapeRight
        let dimsAlreadyMatchOrientation = isLandscape ? (physicalWidth > physicalHeight) : (physicalWidth <= physicalHeight)

        switch orientation {
        case .portrait, .portraitUpsideDown, .faceUp, .faceDown:
            return (physicalWidth, physicalHeight)
        case .landscapeLeft, .landscapeRight:
            return dimsAlreadyMatchOrientation ? (physicalWidth, physicalHeight) : (physicalHeight, physicalWidth)
        }
    }

    /// Maps a logical-screen point (in the user's current orientation) into
    /// the portrait coordinate space expected by `XCPointerEventPath`.
    public static func orientationAwarePoint(
        physicalWidth: Double,
        physicalHeight: Double,
        point: LogicalPoint,
        orientation: LogicalOrientation
    ) -> LogicalPoint {
        let isLandscape = orientation == .landscapeLeft || orientation == .landscapeRight
        let dimsAlreadyMatchOrientation = isLandscape && (physicalWidth > physicalHeight)

        let effectiveWidth = dimsAlreadyMatchOrientation ? physicalHeight : physicalWidth
        let effectiveHeight = dimsAlreadyMatchOrientation ? physicalWidth : physicalHeight

        switch orientation {
        case .portrait:
            return point
        case .portraitUpsideDown:
            return LogicalPoint(x: effectiveWidth - point.x, y: effectiveHeight - point.y)
        case .landscapeLeft:
            return LogicalPoint(x: effectiveWidth - point.y, y: point.x)
        case .landscapeRight:
            return LogicalPoint(x: point.y, y: effectiveHeight - point.x)
        case .faceUp, .faceDown:
            return point
        }
    }

    /// True when `deviceFrame` and `appFrame` represent the same screen size
    /// with width and height swapped (SpringBoard always reports portrait while
    /// a landscape app reports landscape).
    public static func isSameAreaDifferentOrientation(
        deviceWidth: Double,
        deviceHeight: Double,
        appWidth: Double,
        appHeight: Double
    ) -> Bool {
        abs(deviceWidth * deviceHeight - appWidth * appHeight) < 1.0
            && abs(deviceWidth - appHeight) < 1.0
            && abs(deviceHeight - appWidth) < 1.0
    }
}
