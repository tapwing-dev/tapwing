// Pure coordinate math and app-root frame adjustment helpers shared between
// `HierarchyBuilder` (XCUITest) and SwiftPM unit tests (`TapwingRunnerCore`).

import Foundation

public struct FrameOffset: Equatable, Sendable {
    public let dx: Double
    public let dy: Double

    public static let zero = FrameOffset(dx: 0, dy: 0)

    public init(dx: Double, dy: Double) {
        self.dx = dx
        self.dy = dy
    }

    public func adding(_ other: FrameOffset) -> FrameOffset {
        FrameOffset(dx: dx + other.dx, dy: dy + other.dy)
    }
}

public enum HierarchyFrameMath {
    /// Expands `appHierarchy` when the app window frame differs from `deviceFrame`
    /// unless both represent the same screen area in different orientations (e.g.
    /// portrait SpringBoard paired with landscape app).
    public static func adjustAppHierarchy(_ appHierarchy: AXElement, deviceFrame: AXFrame) -> AXElement {
        let appFrame = appHierarchy.frame
        guard deviceFrame != appFrame else {
            return appHierarchy
        }
        guard
            let deviceWidth = deviceFrame["Width"], deviceWidth > 0,
            let deviceHeight = deviceFrame["Height"], deviceHeight > 0,
            let appWidth = appFrame["Width"], appWidth > 0,
            let appHeight = appFrame["Height"], appHeight > 0
        else {
            return appHierarchy
        }

        if OrientationMath.isSameAreaDifferentOrientation(
            deviceWidth: deviceWidth,
            deviceHeight: deviceHeight,
            appWidth: appWidth,
            appHeight: appHeight
        ) {
            return appHierarchy
        }

        let offsetX = deviceWidth - appWidth
        let offsetY = deviceHeight - appHeight
        return expandElementSizes(appHierarchy, offsetX: offsetX, offsetY: offsetY)
    }

    /// Recursively shifts element origin frames by `(offsetX, offsetY)`.
    public static func expandElementSizes(_ element: AXElement, offsetX: Double, offsetY: Double) -> AXElement {
        let adjustedFrame: AXFrame = [
            "X": element.frame.x + offsetX,
            "Y": element.frame.y + offsetY,
            "Width": element.frame.width,
            "Height": element.frame.height,
        ]
        let adjustedChildren = element.children?.map { expandElementSizes($0, offsetX: offsetX, offsetY: offsetY) } ?? []
        return AXElement(
            identifier: element.identifier,
            frame: adjustedFrame,
            value: element.value,
            title: element.title,
            label: element.label,
            elementType: element.elementType,
            enabled: element.enabled,
            horizontalSizeClass: element.horizontalSizeClass,
            verticalSizeClass: element.verticalSizeClass,
            placeholderValue: element.placeholderValue,
            selected: element.selected,
            hasFocus: element.hasFocus,
            displayID: element.displayID,
            windowContextID: element.windowContextID,
            children: adjustedChildren
        )
    }

    /// Calculates the origin delta when transitioning across window context boundaries
    /// into a remote subtree (e.g. out-of-process system/Safari sheets).
    public static func crossProcessWindowOffset(
        rawFrame: AXFrame,
        parentWindowContextID: Double?,
        windowContextID: Double,
        hasRemoteSubtree: Bool,
        visibleFrame: AXFrame?
    ) -> FrameOffset {
        guard
            let parentWindowContextID,
            parentWindowContextID != 0,
            windowContextID != 0,
            parentWindowContextID != windowContextID,
            hasRemoteSubtree,
            let visibleFrame
        else {
            return .zero
        }
        return FrameOffset(dx: visibleFrame.x - rawFrame.x, dy: visibleFrame.y - rawFrame.y)
    }

    /// Translates `frame` by `offset` while preserving width and height.
    public static func offsetFrame(_ frame: AXFrame, by offset: FrameOffset) -> AXFrame {
        guard offset != .zero else { return frame }
        return [
            "X": frame.x + offset.dx,
            "Y": frame.y + offset.dy,
            "Width": frame.width,
            "Height": frame.height,
        ]
    }
}
