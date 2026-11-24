import Foundation
import XCTest
@testable import TapwingRunnerCore

/// Orientation-aware screen sizes and point rotation for every orientation.
final class OrientationMathEdgeTests: XCTestCase {

    private let portraitW = 402.0
    private let portraitH = 874.0

    private func size(_ w: Double, _ h: Double, _ o: LogicalOrientation) -> (Double, Double) {
        let s = OrientationMath.actualScreenSize(physicalWidth: w, physicalHeight: h, orientation: o)
        return (s.width, s.height)
    }

    private func point(_ w: Double, _ h: Double, _ p: LogicalPoint, _ o: LogicalOrientation) -> LogicalPoint {
        OrientationMath.orientationAwarePoint(physicalWidth: w, physicalHeight: h, point: p, orientation: o)
    }

    // MARK: - actualScreenSize

    func testPortraitLikeOrientationsNeverSwapTheDimensions() {
        for o in [LogicalOrientation.portrait, .portraitUpsideDown, .faceUp, .faceDown] {
            XCTAssertTrue(size(portraitW, portraitH, o) == (portraitW, portraitH), "\(o) with portrait dims")
            // Even landscape-shaped dims are passed through unchanged.
            XCTAssertTrue(size(portraitH, portraitW, o) == (portraitH, portraitW), "\(o) with landscape dims")
        }
    }

    func testLandscapeOrientationsSwapPortraitDimsExactlyOnce() {
        for o in [LogicalOrientation.landscapeLeft, .landscapeRight] {
            XCTAssertTrue(size(portraitW, portraitH, o) == (portraitH, portraitW), "\(o) from portrait dims")
            XCTAssertTrue(size(portraitH, portraitW, o) == (portraitH, portraitW), "\(o) already swapped")
        }
    }

    func testSquareScreensAreLeftAlone() {
        for o in [LogicalOrientation.portrait, .landscapeLeft, .landscapeRight, .faceUp] {
            XCTAssertTrue(size(500, 500, o) == (500, 500), "\(o)")
        }
    }

    func testZeroSizedScreensDoNotCrashOrInventDimensions() {
        for o in [LogicalOrientation.portrait, .landscapeLeft, .landscapeRight, .portraitUpsideDown, .faceUp, .faceDown] {
            XCTAssertTrue(size(0, 0, o) == (0, 0), "\(o)")
        }
    }

    // MARK: - orientationAwarePoint

    func testPortraitAndFaceOrientationsReturnThePointUnchanged() {
        let p = LogicalPoint(x: 123.5, y: 456.25)
        for o in [LogicalOrientation.portrait, .faceUp, .faceDown] {
            XCTAssertEqual(point(portraitW, portraitH, p, o), p, "\(o)")
        }
    }

    func testUpsideDownMirrorsBothAxes() {
        XCTAssertEqual(point(portraitW, portraitH, LogicalPoint(x: 0, y: 0), .portraitUpsideDown), LogicalPoint(x: 402, y: 874))
        XCTAssertEqual(point(portraitW, portraitH, LogicalPoint(x: 402, y: 874), .portraitUpsideDown), LogicalPoint(x: 0, y: 0))
        XCTAssertEqual(point(portraitW, portraitH, LogicalPoint(x: 201, y: 437), .portraitUpsideDown), LogicalPoint(x: 201, y: 437))
    }

    func testUpsideDownIsItsOwnInverse() {
        for p in [LogicalPoint(x: 0, y: 0), LogicalPoint(x: 10, y: 20), LogicalPoint(x: 401.5, y: 873.5)] {
            let once = point(portraitW, portraitH, p, .portraitUpsideDown)
            XCTAssertEqual(point(portraitW, portraitH, once, .portraitUpsideDown), p)
        }
    }

    func testLandscapeCornersMapToPortraitCorners() {
        // In landscape the visible screen is 874 wide and 402 tall.
        let corners: [(LogicalPoint, LogicalOrientation, LogicalPoint)] = [
            // landscapeLeft: x' = W - y, y' = x
            (LogicalPoint(x: 0, y: 0), .landscapeLeft, LogicalPoint(x: 402, y: 0)),
            (LogicalPoint(x: 874, y: 0), .landscapeLeft, LogicalPoint(x: 402, y: 874)),
            (LogicalPoint(x: 0, y: 402), .landscapeLeft, LogicalPoint(x: 0, y: 0)),
            (LogicalPoint(x: 874, y: 402), .landscapeLeft, LogicalPoint(x: 0, y: 874)),
            // landscapeRight: x' = y, y' = H - x
            (LogicalPoint(x: 0, y: 0), .landscapeRight, LogicalPoint(x: 0, y: 874)),
            (LogicalPoint(x: 874, y: 0), .landscapeRight, LogicalPoint(x: 0, y: 0)),
            (LogicalPoint(x: 0, y: 402), .landscapeRight, LogicalPoint(x: 402, y: 874)),
            (LogicalPoint(x: 874, y: 402), .landscapeRight, LogicalPoint(x: 402, y: 0)),
        ]
        for (input, orientation, expected) in corners {
            XCTAssertEqual(point(portraitW, portraitH, input, orientation), expected, "\(orientation) \(input)")
            // The result is the same whichever way the snapshot reported the dimensions.
            XCTAssertEqual(point(portraitH, portraitW, input, orientation), expected, "\(orientation) \(input) swapped dims")
        }
    }

    func testEveryPointInsideTheLandscapeScreenLandsInsideThePortraitScreen() {
        for o in [LogicalOrientation.landscapeLeft, .landscapeRight] {
            for x in stride(from: 0.0, through: 874.0, by: 97.0) {
                for y in stride(from: 0.0, through: 402.0, by: 67.0) {
                    let mapped = point(portraitW, portraitH, LogicalPoint(x: x, y: y), o)
                    XCTAssertTrue((0...portraitW).contains(mapped.x), "\(o) (\(x),\(y)) -> x \(mapped.x)")
                    XCTAssertTrue((0...portraitH).contains(mapped.y), "\(o) (\(x),\(y)) -> y \(mapped.y)")
                }
            }
        }
    }

    func testTheTwoLandscapeOrientationsAreOppositeRotations() {
        // A point rotated left and the same point rotated right end up on opposite sides of the screen.
        for p in [LogicalPoint(x: 100, y: 50), LogicalPoint(x: 700, y: 300), LogicalPoint(x: 437, y: 201)] {
            let left = point(portraitW, portraitH, p, .landscapeLeft)
            let right = point(portraitW, portraitH, p, .landscapeRight)
            XCTAssertEqual(left.x + right.x, portraitW, accuracy: 1e-9)
            XCTAssertEqual(left.y + right.y, portraitH, accuracy: 1e-9)
        }
    }

    func testFractionalPointsKeepTheirPrecision() {
        XCTAssertEqual(
            point(portraitW, portraitH, LogicalPoint(x: 100.25, y: 50.75), .landscapeLeft),
            LogicalPoint(x: 351.25, y: 100.25)
        )
    }

    func testTheCenterOfASquareScreenIsAFixedPointOfEveryRotation() {
        let center = LogicalPoint(x: 250, y: 250)
        for o in [LogicalOrientation.portrait, .portraitUpsideDown, .landscapeLeft, .landscapeRight, .faceUp, .faceDown] {
            XCTAssertEqual(point(500, 500, center, o), center, "\(o)")
        }
    }

    // MARK: - isSameAreaDifferentOrientation

    func testSameAreaDetectionHonoursItsOnePointTolerance() {
        func same(_ dw: Double, _ dh: Double, _ aw: Double, _ ah: Double) -> Bool {
            OrientationMath.isSameAreaDifferentOrientation(deviceWidth: dw, deviceHeight: dh, appWidth: aw, appHeight: ah)
        }
        XCTAssertTrue(same(402, 874, 874, 402))
        // The area check is strict (< 1 square point), so only float noise passes.
        XCTAssertTrue(same(402, 874, 874.0004, 402), "floating point noise")
        XCTAssertFalse(same(402, 874, 874.5, 402.4), "half a point off changes the area by hundreds")
        XCTAssertFalse(same(402, 874, 875.5, 402), "wider than the device is tall")
        XCTAssertFalse(same(402, 874, 402, 874), "identical orientation is not a rotation")
        XCTAssertFalse(same(402, 874, 874, 403), "area differs by more than a point")
        XCTAssertFalse(same(402, 874, 300, 600))
    }

    func testSameAreaDetectionForSquareAndZeroScreens() {
        func same(_ dw: Double, _ dh: Double, _ aw: Double, _ ah: Double) -> Bool {
            OrientationMath.isSameAreaDifferentOrientation(deviceWidth: dw, deviceHeight: dh, appWidth: aw, appHeight: ah)
        }
        XCTAssertTrue(same(500, 500, 500, 500), "a square screen is its own rotation")
        XCTAssertTrue(same(0, 0, 0, 0))
        XCTAssertFalse(same(0, 874, 874, 402))
    }

    func testLogicalPointAndOrientationAreValueTypes() {
        XCTAssertEqual(LogicalPoint(x: 1, y: 2), LogicalPoint(x: 1, y: 2))
        XCTAssertNotEqual(LogicalPoint(x: 1, y: 2), LogicalPoint(x: 2, y: 1))
        XCTAssertNotEqual(LogicalOrientation.landscapeLeft, .landscapeRight)
        XCTAssertEqual(LogicalOrientation.faceUp, .faceUp)
    }
}
