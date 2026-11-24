// Accessibility hierarchy snapshot builder for Tapwing's iOS Simulator runner.
// Ported from Maestro 2.12.1 (`Routes/Handlers/ViewHierarchyHandler.swift`,
// Apache-2.0, Copyright (c) 2022 mobile.dev inc., modified for Tapwing).

import Foundation
import XCTest

@MainActor
struct HierarchyBuilder {
    private let springboardApplication = XCUIApplication(bundleIdentifier: RunningApp.springboardBundleId)
    private let snapshotMaxDepth = 60

    func handle(_ request: HttpRequest) async -> HttpResponse {
        guard let body = try? JSONDecoder().decode(ViewHierarchyRequest.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body provided for viewHierarchy").httpResponse
        }

        do {
            guard let foreground = RunningApp.getForegroundAppInfo() else {
                let springboardHierarchy = try elementHierarchy(xcuiElement: springboardApplication)
                let response = ViewHierarchyResponse(
                    axElement: springboardHierarchy,
                    depth: springboardHierarchy.depth()
                )
                return HttpResponse.json(response)
            }

            let appViewHierarchy = try getAppViewHierarchy(
                foregroundApp: foreground.app,
                foregroundBundleId: foreground.bundleId,
                excludeKeyboardElements: body.excludeKeyboardElements ?? false
            )
            let response = ViewHierarchyResponse(
                axElement: appViewHierarchy,
                depth: appViewHierarchy.depth()
            )
            return HttpResponse.json(response)
        } catch let error as AppError {
            return error.httpResponse
        } catch {
            if (error as NSError).isXCUITestTimeout {
                return AppError(type: .timeout, message: "Snapshot failure while getting view hierarchy. Error: \(error.localizedDescription)").httpResponse
            }
            return AppError(message: "Snapshot failure while getting view hierarchy. Error: \(error.localizedDescription)").httpResponse
        }
    }

    private func getAppViewHierarchy(
        foregroundApp originalForegroundApp: XCUIApplication,
        foregroundBundleId: String,
        excludeKeyboardElements _: Bool
    ) throws -> AXElement {
        // On iOS 26+ iPad, `DockFolderViewService` can report as foreground on
        // the home screen while its AX tree is empty; actual content lives in SpringBoard.
        var foregroundApp = originalForegroundApp
        if foregroundBundleId == "com.apple.DocumentManager.DockFolderViewService" {
            foregroundApp = springboardApplication
        }

        let appHierarchy = try getHierarchyWithFallback(foregroundApp)
        let statusBars = fullStatusBars(springboardApplication) ?? []
        let safariWebViewHierarchy = getSafariWebViewHierarchy()

        let deviceFrame = springboardApplication.frame
        let deviceAxFrame: AXFrame = [
            "X": Double(deviceFrame.minX),
            "Y": Double(deviceFrame.minY),
            "Width": Double(deviceFrame.width),
            "Height": Double(deviceFrame.height),
        ]
        let appFrame = appHierarchy.frame

        if deviceAxFrame != appFrame {
            guard
                let deviceWidth = deviceAxFrame["Width"], deviceWidth > 0,
                let deviceHeight = deviceAxFrame["Height"], deviceHeight > 0,
                let appWidth = appFrame["Width"], appWidth > 0,
                let appHeight = appFrame["Height"], appHeight > 0
            else {
                return wrapRoot(appHierarchy, statusBars: statusBars, safari: safariWebViewHierarchy)
            }

            // SpringBoard reports portrait dimensions even when the app is in landscape.
            if OrientationMath.isSameAreaDifferentOrientation(
                deviceWidth: deviceWidth,
                deviceHeight: deviceHeight,
                appWidth: appWidth,
                appHeight: appHeight
            ) {
                return wrapRoot(appHierarchy, statusBars: statusBars, safari: safariWebViewHierarchy)
            }

            let offsetX = deviceWidth - appWidth
            let offsetY = deviceHeight - appHeight
            let adjusted = expandElementSizes(appHierarchy, offsetX: offsetX, offsetY: offsetY)
            return wrapRoot(adjusted, statusBars: statusBars, safari: safariWebViewHierarchy)
        }

        return wrapRoot(appHierarchy, statusBars: statusBars, safari: safariWebViewHierarchy)
    }

    private func wrapRoot(_ app: AXElement, statusBars: [AXElement], safari: AXElement?) -> AXElement {
        var children: [AXElement] = [app, AXElement(children: statusBars)]
        if let safari {
            children.append(safari)
        }
        return AXElement(children: children)
    }

    private func expandElementSizes(_ element: AXElement, offsetX: Double, offsetY: Double) -> AXElement {
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

    private func getHierarchyWithFallback(
        _ element: XCUIElement,
        recursionDepth: Int = 0,
        allowRecovery: Bool = true
    ) throws -> AXElement {
        do {
            var hierarchy = try elementHierarchy(xcuiElement: element)
            if hierarchy.depth() < snapshotMaxDepth || recursionDepth >= 8 {
                return hierarchy
            }
            let count = try element.snapshot().children.count
            var children: [AXElement] = []
            children.reserveCapacity(count)
            for i in 0..<count {
                let child = element.descendants(matching: .other).element(boundBy: i).firstMatch
                children.append(
                    try getHierarchyWithFallback(
                        child,
                        recursionDepth: recursionDepth + 1,
                        allowRecovery: allowRecovery
                    )
                )
            }
            hierarchy.children = children
            return hierarchy
        } catch {
            guard allowRecovery, recursionDepth < 8, isRecoverableSnapshotError(error) else {
                if (error as NSError).isXCUITestTimeout {
                    throw AppError(type: .timeout, message: error.localizedDescription)
                }
                throw AppError(message: error.localizedDescription)
            }

            TapwingSetSnapshotParameter("maxDepth", NSNumber(value: snapshotMaxDepth))
            defer { TapwingRemoveSnapshotParameter("maxDepth") }
            let recoveryElement = try findRecoveryElement(element.children(matching: .any).firstMatch)
            let hierarchy = try getHierarchyWithFallback(
                recoveryElement,
                recursionDepth: recursionDepth + 1,
                allowRecovery: false
            )

            if let app = element as? XCUIApplication {
                let keyboard = keyboardHierarchy(app)
                let alerts = fullScreenAlertHierarchy(app)
                let other = try customWindowElements(app)
                return AXElement(children: [other, keyboard, alerts, hierarchy].compactMap { $0 })
            }
            return hierarchy
        }
    }

    private func isRecoverableSnapshotError(_ error: Error) -> Bool {
        let description = error.localizedDescription
        return description.contains("Error kAXErrorIllegalArgument getting snapshot for element")
            || description.contains("kAXErrorInvalidUIElement")
    }

    private func keyboardHierarchy(_ app: XCUIApplication) -> AXElement? {
        let keyboard = app.keyboards.firstMatch
        guard keyboard.exists else { return nil }
        return try? elementHierarchy(xcuiElement: keyboard)
    }

    private func customWindowElements(_ app: XCUIApplication) throws -> AXElement? {
        let windowElement = app.children(matching: .any).firstMatch
        if try windowElement.snapshot().children.count > 1 {
            return nil
        }
        return try? elementHierarchy(xcuiElement: windowElement)
    }

    private func fullScreenAlertHierarchy(_ app: XCUIApplication) -> AXElement? {
        let alert = app.alerts.firstMatch
        guard alert.exists else { return nil }
        return try? elementHierarchy(xcuiElement: alert)
    }

    private func fullStatusBars(_ app: XCUIApplication) -> [AXElement]? {
        guard app.statusBars.firstMatch.exists else { return nil }
        return try? app.statusBars.allElementsBoundByIndex.compactMap { statusBar in
            try elementHierarchy(xcuiElement: statusBar)
        }
    }

    /// Merges the separate `com.apple.SafariViewService` WebView process on iOS 26+
    /// so `SFSafariViewController` / `ASWebAuthenticationSession` DOM elements are visible.
    private func getSafariWebViewHierarchy() -> AXElement? {
        guard ProcessInfo.processInfo.operatingSystemVersion.majorVersion >= 26 else {
            return nil
        }
        let safariService = XCUIApplication(bundleIdentifier: "com.apple.SafariViewService")
        let isRunning = safariService.state == .runningForeground || safariService.state == .runningBackground
        guard isRunning, safariService.webViews.count > 0 else {
            return nil
        }
        do {
            TapwingSetSnapshotParameter("maxDepth", NSNumber(value: snapshotMaxDepth))
            defer { TapwingRemoveSnapshotParameter("maxDepth") }
            return try elementHierarchy(xcuiElement: safariService)
        } catch {
            NSLog("[TapwingRunner] SafariViewService snapshot failed: %@", error.localizedDescription)
            return nil
        }
    }

    private func findRecoveryElement(_ element: XCUIElement, depth: Int = 0) throws -> XCUIElement {
        if depth >= 20 {
            return element
        }
        if try element.snapshot().children.count > 1 {
            return element
        }
        let firstOther = element.children(matching: .other).firstMatch
        if firstOther.exists {
            return try findRecoveryElement(firstOther, depth: depth + 1)
        }
        return element
    }

    private func elementHierarchy(xcuiElement: XCUIElement) throws -> AXElement {
        let snapshot = try xcuiElement.snapshot()
        return elementHierarchy(snapshot: snapshot, inheritedOffset: .zero, parentWindowContextID: nil)
    }

    /// Walks `XCUIElementSnapshot` directly (avoiding O(subtree) `dictionaryRepresentation`)
    /// and applies cross-process window offsets when entering remote subtrees.
    private func elementHierarchy(
        snapshot: XCUIElementSnapshot,
        inheritedOffset: CGVector,
        parentWindowContextID: Double?
    ) -> AXElement {
        let rawFrame = axFrame(from: snapshot.frame)
        let windowContextID = kvcDouble(snapshot, "windowContextID") ?? 0

        let boundaryOffset = crossProcessWindowOffset(
            snapshot: snapshot,
            rawFrame: rawFrame,
            parentWindowContextID: parentWindowContextID,
            windowContextID: windowContextID
        )
        let currentOffset = CGVector(
            dx: inheritedOffset.dx + boundaryOffset.dx,
            dy: inheritedOffset.dy + boundaryOffset.dy
        )

        let children = snapshot.children.map { child in
            elementHierarchy(
                snapshot: child,
                inheritedOffset: currentOffset,
                parentWindowContextID: windowContextID
            )
        }

        return AXElement(
            identifier: snapshot.identifier,
            frame: offsetFrame(rawFrame, by: currentOffset),
            value: snapshot.value as? String,
            title: snapshot.title,
            label: snapshot.label,
            elementType: Int(snapshot.elementType.rawValue),
            enabled: snapshot.isEnabled,
            horizontalSizeClass: snapshot.horizontalSizeClass.rawValue,
            verticalSizeClass: snapshot.verticalSizeClass.rawValue,
            placeholderValue: snapshot.placeholderValue,
            selected: snapshot.isSelected,
            hasFocus: snapshot.hasFocus,
            displayID: kvcInt(snapshot, "displayID") ?? 0,
            windowContextID: windowContextID,
            children: children
        )
    }

    private func crossProcessWindowOffset(
        snapshot: XCUIElementSnapshot,
        rawFrame: AXFrame,
        parentWindowContextID: Double?,
        windowContextID: Double
    ) -> CGVector {
        guard
            let parentWindowContextID,
            parentWindowContextID != 0,
            windowContextID != 0,
            parentWindowContextID != windowContextID,
            containsRemoteSubtree(snapshot),
            let visibleFrame = visibleFrame(snapshot)
        else {
            return .zero
        }
        return CGVector(dx: visibleFrame.x - rawFrame.x, dy: visibleFrame.y - rawFrame.y)
    }

    private func containsRemoteSubtree(_ snapshot: XCUIElementSnapshot) -> Bool {
        if isRemote(snapshot) { return true }
        return snapshot.children.contains(where: isRemote)
    }

    private func isRemote(_ snapshot: XCUIElementSnapshot) -> Bool {
        guard let obj = snapshot as? NSObject,
              obj.responds(to: NSSelectorFromString("isRemote")) else {
            return false
        }
        return (obj.value(forKey: "isRemote") as? NSNumber)?.boolValue ?? false
    }

    private func visibleFrame(_ snapshot: XCUIElementSnapshot) -> AXFrame? {
        guard let obj = snapshot as? NSObject,
              obj.responds(to: NSSelectorFromString("visibleFrame")),
              let value = obj.value(forKey: "visibleFrame") as? NSValue else {
            return nil
        }
        let rect = value.cgRectValue
        guard rect.origin.x.isFinite,
              rect.origin.y.isFinite,
              rect.size.width.isFinite,
              rect.size.height.isFinite else {
            return nil
        }
        return axFrame(from: rect)
    }

    private func axFrame(from rect: CGRect) -> AXFrame {
        [
            "X": Double(rect.origin.x),
            "Y": Double(rect.origin.y),
            "Width": Double(rect.size.width),
            "Height": Double(rect.size.height),
        ]
    }

    private func kvcDouble(_ snapshot: XCUIElementSnapshot, _ key: String) -> Double? {
        guard let obj = snapshot as? NSObject,
              obj.responds(to: NSSelectorFromString(key)) else {
            return nil
        }
        return (obj.value(forKey: key) as? NSNumber)?.doubleValue
    }

    private func kvcInt(_ snapshot: XCUIElementSnapshot, _ key: String) -> Int? {
        guard let obj = snapshot as? NSObject,
              obj.responds(to: NSSelectorFromString(key)) else {
            return nil
        }
        return (obj.value(forKey: key) as? NSNumber)?.intValue
    }

    private func offsetFrame(_ frame: AXFrame, by offset: CGVector) -> AXFrame {
        guard offset.dx != 0 || offset.dy != 0 else { return frame }
        return [
            "X": frame.x + Double(offset.dx),
            "Y": frame.y + Double(offset.dy),
            "Width": frame.width,
            "Height": frame.height,
        ]
    }
}
