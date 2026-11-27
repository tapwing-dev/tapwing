import Foundation
import XCTest
@testable import TapwingRunnerCore

final class HierarchyFrameMathTests: XCTestCase {

    private func node(frame: AXFrame, children: [AXElement] = []) -> AXElement {
        AXElement(identifier: "node", frame: frame, label: "label", elementType: 9, enabled: true, children: children)
    }

    func testAdjustAppHierarchyLeavesEqualFramesAndOrientationSwapsUntouched() {
        let leaf = node(frame: ["X": 10, "Y": 20, "Width": 50, "Height": 30])
        let app = node(frame: ["X": 0, "Y": 0, "Width": 402, "Height": 874], children: [leaf])

        // Equal frames: unchanged.
        let same = HierarchyFrameMath.adjustAppHierarchy(
            app,
            deviceFrame: ["X": 0, "Y": 0, "Width": 402, "Height": 874]
        )
        XCTAssertEqual(same, app)

        // SpringBoard still reporting portrait dimensions while app is landscape: unchanged.
        let landscapeApp = node(frame: ["X": 0, "Y": 0, "Width": 874, "Height": 402], children: [leaf])
        let unchangedLandscape = HierarchyFrameMath.adjustAppHierarchy(
            landscapeApp,
            deviceFrame: ["X": 0, "Y": 0, "Width": 402, "Height": 874]
        )
        XCTAssertEqual(unchangedLandscape, landscapeApp)
    }

    func testAdjustAppHierarchyIgnoresNonPositiveDimensions() {
        let app = node(frame: ["X": 0, "Y": 0, "Width": 0, "Height": 874])
        let out = HierarchyFrameMath.adjustAppHierarchy(
            app,
            deviceFrame: ["X": 0, "Y": 0, "Width": 402, "Height": 874]
        )
        XCTAssertEqual(out, app)

        let zeroDevice = node(frame: ["X": 0, "Y": 0, "Width": 402, "Height": 874])
        XCTAssertEqual(
            HierarchyFrameMath.adjustAppHierarchy(
                zeroDevice,
                deviceFrame: ["X": 0, "Y": 0, "Width": -1, "Height": 874]
            ),
            zeroDevice
        )
    }

    func testAdjustAppHierarchyShiftsEntireSubtreeByDimensionDelta() {
        let grandChild = node(frame: ["X": 5, "Y": 10, "Width": 20, "Height": 20])
        let child = node(frame: ["X": 10, "Y": 20, "Width": 100, "Height": 40], children: [grandChild])
        let app = node(frame: ["X": 0, "Y": 0, "Width": 390, "Height": 844], children: [child])

        let adjusted = HierarchyFrameMath.adjustAppHierarchy(
            app,
            deviceFrame: ["X": 0, "Y": 0, "Width": 402, "Height": 874]
        )
        XCTAssertEqual(adjusted.frame, ["X": 12, "Y": 30, "Width": 390, "Height": 844])
        XCTAssertEqual(adjusted.children?[0].frame, ["X": 22, "Y": 50, "Width": 100, "Height": 40])
        XCTAssertEqual(adjusted.children?[0].children?[0].frame, ["X": 17, "Y": 40, "Width": 20, "Height": 20])
    }

    func testCrossProcessWindowOffsetComputesDeltaOnlyAcrossRemoteBoundaries() {
        let raw: AXFrame = ["X": 0, "Y": 0, "Width": 402, "Height": 800]
        let visible: AXFrame = ["X": 16, "Y": 54, "Width": 370, "Height": 740]

        XCTAssertEqual(
            HierarchyFrameMath.crossProcessWindowOffset(
                rawFrame: raw,
                parentWindowContextID: 100,
                windowContextID: 200,
                hasRemoteSubtree: true,
                visibleFrame: visible
            ),
            FrameOffset(dx: 16, dy: 54)
        )

        // Same window context -> no offset.
        XCTAssertEqual(
            HierarchyFrameMath.crossProcessWindowOffset(
                rawFrame: raw,
                parentWindowContextID: 100,
                windowContextID: 100,
                hasRemoteSubtree: true,
                visibleFrame: visible
            ),
            .zero
        )

        // Zero or missing parent context -> no offset.
        XCTAssertEqual(
            HierarchyFrameMath.crossProcessWindowOffset(
                rawFrame: raw,
                parentWindowContextID: nil,
                windowContextID: 200,
                hasRemoteSubtree: true,
                visibleFrame: visible
            ),
            .zero
        )
        XCTAssertEqual(
            HierarchyFrameMath.crossProcessWindowOffset(
                rawFrame: raw,
                parentWindowContextID: 0,
                windowContextID: 200,
                hasRemoteSubtree: true,
                visibleFrame: visible
            ),
            .zero
        )

        // Local subtree or missing visibleFrame -> no offset.
        XCTAssertEqual(
            HierarchyFrameMath.crossProcessWindowOffset(
                rawFrame: raw,
                parentWindowContextID: 100,
                windowContextID: 200,
                hasRemoteSubtree: false,
                visibleFrame: visible
            ),
            .zero
        )
        XCTAssertEqual(
            HierarchyFrameMath.crossProcessWindowOffset(
                rawFrame: raw,
                parentWindowContextID: 100,
                windowContextID: 200,
                hasRemoteSubtree: true,
                visibleFrame: nil
            ),
            .zero
        )
    }

    func testOffsetFrameTranslatesOriginAndComposesOffsets() {
        let raw: AXFrame = ["X": 10, "Y": 20, "Width": 120, "Height": 44]
        let combined = FrameOffset(dx: 5, dy: -3).adding(FrameOffset(dx: 11, dy: 57))
        XCTAssertEqual(
            HierarchyFrameMath.offsetFrame(raw, by: combined),
            ["X": 26, "Y": 74, "Width": 120, "Height": 44]
        )
        XCTAssertEqual(HierarchyFrameMath.offsetFrame(raw, by: .zero), raw)
    }
}
