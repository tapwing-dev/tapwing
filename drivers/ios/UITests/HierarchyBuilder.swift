// Accessibility hierarchy snapshot builder for Tapwing's iOS Simulator runner.
// Ported from Maestro 2.12.1 (`Routes/Handlers/ViewHierarchyHandler.swift`,
// Apache-2.0, Copyright (c) 2022 mobile.dev inc., modified for Tapwing).

import Foundation
import XCTest

@MainActor
final class HierarchyBuilder {
    private let springboardApplication = XCUIApplication(bundleIdentifier: RunningApp.springboardBundleId)
    private let snapshotMaxDepth = 60
    private var cache = HierarchyMetadataCache()

    private static let isRemoteSelector = NSSelectorFromString("isRemote")
    private static let visibleFrameSelector = NSSelectorFromString("visibleFrame")
    private static let windowContextIDSelector = NSSelectorFromString("windowContextID")
    private static let displayIDSelector = NSSelectorFromString("displayID")

    func invalidateCache() {
        cache.invalidateAll()
    }

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
        // Retained for Maestro 2.12.1 `ViewHierarchyRequest` wire compatibility.
        excludeKeyboardElements _: Bool
    ) throws -> AXElement {
        let isSpringboardTarget = HierarchyQueryPolicy.isSpringboardTarget(foregroundBundleId: foregroundBundleId)

        // On iOS 26+ iPad, `DockFolderViewService` can report as foreground on
        // the home screen while its AX tree is empty; actual content lives in SpringBoard.
        let foregroundApp = isSpringboardTarget ? springboardApplication : originalForegroundApp

        var hasRemoteElement = false
        let appHierarchy = try getHierarchyWithFallback(foregroundApp, hasRemoteElement: &hasRemoteElement)

        let safariWebViewHierarchy: AXElement?
        if HierarchyQueryPolicy.shouldQuerySafariViewService(
            osMajorVersion: ProcessInfo.processInfo.operatingSystemVersion.majorVersion,
            foregroundBundleId: foregroundBundleId,
            hasRemoteElement: hasRemoteElement
        ) {
            safariWebViewHierarchy = getSafariWebViewHierarchy()
        } else {
            safariWebViewHierarchy = nil
        }

        // When SpringBoard itself is the foreground snapshot target, `appHierarchy`
        // already includes status bars and SpringBoard's frame — skip both extra IPCs.
        if isSpringboardTarget {
            return wrapRoot(appHierarchy, statusBars: [], safari: safariWebViewHierarchy)
        }

        let orientation = ScreenSizeHelper.logicalOrientation()
        let statusBars = cache.statusBars(
            orientation: orientation,
            foregroundBundleId: foregroundBundleId
        ) { [self] in
            fullStatusBars(springboardApplication)
        }

        let deviceAxFrame = cache.deviceFrame(for: orientation) { [self] in
            axFrame(from: springboardApplication.frame)
        }
        let adjusted = HierarchyFrameMath.adjustAppHierarchy(appHierarchy, deviceFrame: deviceAxFrame)
        return wrapRoot(adjusted, statusBars: statusBars, safari: safariWebViewHierarchy)
    }

    private func wrapRoot(_ app: AXElement, statusBars: [AXElement], safari: AXElement?) -> AXElement {
        var children: [AXElement] = [app, AXElement(children: statusBars)]
        if let safari {
            children.append(safari)
        }
        return AXElement(children: children)
    }

    private func getHierarchyWithFallback(
        _ element: XCUIElement,
        recursionDepth: Int = 0,
        allowRecovery: Bool = true,
        hasRemoteElement: inout Bool
    ) throws -> AXElement {
        do {
            var hierarchy = try elementHierarchy(xcuiElement: element, hasRemoteElement: &hasRemoteElement)
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
                        allowRecovery: allowRecovery,
                        hasRemoteElement: &hasRemoteElement
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
                allowRecovery: false,
                hasRemoteElement: &hasRemoteElement
            )

            if let app = element as? XCUIApplication {
                let keyboard = keyboardHierarchy(app)
                let alerts = fullScreenAlertHierarchy(app)
                let other = try customWindowElements(app, hasRemoteElement: &hasRemoteElement)
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

    private func customWindowElements(_ app: XCUIApplication, hasRemoteElement: inout Bool) throws -> AXElement? {
        let windowElement = app.children(matching: .any).firstMatch
        if try windowElement.snapshot().children.count > 1 {
            return nil
        }
        return try? elementHierarchy(xcuiElement: windowElement, hasRemoteElement: &hasRemoteElement)
    }

    private func fullScreenAlertHierarchy(_ app: XCUIApplication) -> AXElement? {
        let alert = app.alerts.firstMatch
        guard alert.exists else { return nil }
        return try? elementHierarchy(xcuiElement: alert)
    }

    private func fullStatusBars(_ app: XCUIApplication) -> [AXElement]? {
        // Call `allElementsBoundByIndex` directly (returns `[]` when empty) to
        // avoid an extra `statusBars.firstMatch.exists` SpringBoard AX IPC.
        let statusBarElements = app.statusBars.allElementsBoundByIndex
        if statusBarElements.isEmpty {
            return []
        }
        do {
            return try statusBarElements.map { statusBar in
                try elementHierarchy(xcuiElement: statusBar)
            }
        } catch {
            return nil
        }
    }

    /// Merges the separate `com.apple.SafariViewService` WebView process on iOS 26+
    /// so `SFSafariViewController` / `ASWebAuthenticationSession` DOM elements are visible.
    private func getSafariWebViewHierarchy() -> AXElement? {
        guard ProcessInfo.processInfo.operatingSystemVersion.majorVersion >= 26 else {
            return nil
        }
        let safariService = XCUIApplication(bundleIdentifier: HierarchyQueryPolicy.safariViewServiceBundleId)
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
        var ignoredRemote = false
        return try elementHierarchy(xcuiElement: xcuiElement, hasRemoteElement: &ignoredRemote)
    }

    private func elementHierarchy(xcuiElement: XCUIElement, hasRemoteElement: inout Bool) throws -> AXElement {
        let snapshot = try xcuiElement.snapshot()
        return elementHierarchy(
            snapshot: snapshot,
            inheritedOffset: .zero,
            parentWindowContextID: nil,
            hasRemoteElement: &hasRemoteElement
        )
    }

    /// Walks `XCUIElementSnapshot` directly (avoiding O(subtree) `dictionaryRepresentation`)
    /// and applies cross-process window offsets when entering remote subtrees.
    private func elementHierarchy(
        snapshot: XCUIElementSnapshot,
        inheritedOffset: FrameOffset,
        parentWindowContextID: Double?,
        hasRemoteElement: inout Bool
    ) -> AXElement {
        let rawFrame = axFrame(from: snapshot.frame)
        let windowContextID = kvcDouble(snapshot, selector: Self.windowContextIDSelector, key: "windowContextID") ?? 0
        let nodeIsRemote = isRemote(snapshot)
        if nodeIsRemote {
            hasRemoteElement = true
        }

        let boundaryOffset = HierarchyFrameMath.crossProcessWindowOffset(
            rawFrame: rawFrame,
            parentWindowContextID: parentWindowContextID,
            windowContextID: windowContextID,
            hasRemoteSubtree: containsRemoteSubtree(snapshot, nodeIsRemote: nodeIsRemote),
            visibleFrame: visibleFrame(snapshot)
        )
        let currentOffset = inheritedOffset.adding(boundaryOffset)

        let children = snapshot.children.map { child in
            elementHierarchy(
                snapshot: child,
                inheritedOffset: currentOffset,
                parentWindowContextID: windowContextID,
                hasRemoteElement: &hasRemoteElement
            )
        }

        return AXElement(
            identifier: snapshot.identifier,
            frame: HierarchyFrameMath.offsetFrame(rawFrame, by: currentOffset),
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
            displayID: kvcInt(snapshot, selector: Self.displayIDSelector, key: "displayID") ?? 0,
            windowContextID: windowContextID,
            children: children
        )
    }

    private func containsRemoteSubtree(_ snapshot: XCUIElementSnapshot, nodeIsRemote: Bool) -> Bool {
        if nodeIsRemote { return true }
        return snapshot.children.contains(where: isRemote)
    }

    private func isRemote(_ snapshot: XCUIElementSnapshot) -> Bool {
        guard let obj = snapshot as? NSObject,
              obj.responds(to: Self.isRemoteSelector) else {
            return false
        }
        return (obj.value(forKey: "isRemote") as? NSNumber)?.boolValue ?? false
    }

    private func visibleFrame(_ snapshot: XCUIElementSnapshot) -> AXFrame? {
        guard let obj = snapshot as? NSObject,
              obj.responds(to: Self.visibleFrameSelector),
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

    private func kvcDouble(_ snapshot: XCUIElementSnapshot, selector: Selector, key: String) -> Double? {
        guard let obj = snapshot as? NSObject,
              obj.responds(to: selector) else {
            return nil
        }
        return (obj.value(forKey: key) as? NSNumber)?.doubleValue
    }

    private func kvcInt(_ snapshot: XCUIElementSnapshot, selector: Selector, key: String) -> Int? {
        guard let obj = snapshot as? NSObject,
              obj.responds(to: selector) else {
            return nil
        }
        return (obj.value(forKey: key) as? NSNumber)?.intValue
    }
}
