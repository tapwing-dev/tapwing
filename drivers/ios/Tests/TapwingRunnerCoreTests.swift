import Foundation
import XCTest
@testable import TapwingRunnerCore

final class TapwingRunnerCoreTests: XCTestCase {

    // MARK: - HTTP/1.1 framing & keep-alive

    func testParsesSequentialKeepAliveRequestsFromSingleStream() {
        let touchBody = "{\"x\":201.5,\"y\":437.25}"
        var buffer = Data(
            (
                "GET /status HTTP/1.1\r\nHost: 127.0.0.1:22087\r\n\r\n" +
                "POST /touch HTTP/1.1\r\nHost: 127.0.0.1:22087\r\nContent-Type: application/json\r\nContent-Length: \(touchBody.utf8.count)\r\n\r\n" +
                touchBody
            ).utf8
        )

        guard case .complete(let req1) = HttpFraming.tryParseRequest(from: &buffer) else {
            return XCTFail("Expected first request to parse")
        }
        XCTAssertEqual(req1.method, "GET")
        XCTAssertEqual(req1.path, "/status")
        XCTAssertTrue(req1.body.isEmpty)
        XCTAssertTrue(req1.shouldKeepAlive)

        guard case .complete(let req2) = HttpFraming.tryParseRequest(from: &buffer) else {
            return XCTFail("Expected second pipelined/keep-alive request to parse")
        }
        XCTAssertEqual(req2.method, "POST")
        XCTAssertEqual(req2.path, "/touch")
        XCTAssertEqual(String(data: req2.body, encoding: .utf8), touchBody)
        XCTAssertTrue(req2.shouldKeepAlive)
        XCTAssertTrue(buffer.isEmpty)
    }

    func testHandlesFragmentedHeaderAndBodyArrival() {
        // Content-Length counts UTF-8 bytes, not characters.
        let bodyStr = "{\"text\":\"Příliš kůň!\"}"
        let exact = Data("POST /inputText HTTP/1.1\r\nContent-Length: \(bodyStr.utf8.count)\r\n\r\n\(bodyStr)".utf8)

        var buffer = Data()
        var parsed: HttpRequest?
        for byte in exact {
            buffer.append(byte)
            switch HttpFraming.tryParseRequest(from: &buffer) {
            case .incomplete:
                continue
            case .complete(let req):
                parsed = req
            case .malformed(let err):
                return XCTFail("Unexpected malformed error: \(err)")
            }
        }

        XCTAssertNotNil(parsed)
        XCTAssertEqual(parsed?.path, "/inputText")
        XCTAssertEqual(String(data: parsed?.body ?? Data(), encoding: .utf8), bodyStr)
        XCTAssertTrue(buffer.isEmpty)
    }

    func testParsesQueryStringAndConnectionClose() {
        var buffer = Data(
            "GET /screenshot?compressed=true&unused=hello%20world HTTP/1.1\r\nConnection: close\r\n\r\n".utf8
        )
        guard case .complete(let req) = HttpFraming.tryParseRequest(from: &buffer) else {
            return XCTFail("Expected complete request")
        }
        XCTAssertEqual(req.path, "/screenshot")
        XCTAssertEqual(req.query["compressed"], "true")
        XCTAssertEqual(req.query["unused"], "hello world")
        XCTAssertFalse(req.shouldKeepAlive)
    }

    func testRejectsMalformedContentLengthAndChunkedTransfer() {
        var badLen = Data("POST /touch HTTP/1.1\r\nContent-Length: nope\r\n\r\n".utf8)
        if case .malformed = HttpFraming.tryParseRequest(from: &badLen) {
            // expected
        } else {
            XCTFail("Expected malformed for non-numeric Content-Length")
        }

        var chunked = Data("POST /touch HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".utf8)
        if case .malformed = HttpFraming.tryParseRequest(from: &chunked) {
            // expected
        } else {
            XCTFail("Expected malformed for chunked Transfer-Encoding")
        }
    }

    func testSerializesHttpResponseWithExactContentLength() {
        let resp = HttpResponse.ok(Data("{\"ok\":true}".utf8))
        let wire = String(data: HttpFraming.serialize(resp, keepAlive: true), encoding: .utf8)
        XCTAssertEqual(
            wire,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: keep-alive\r\n\r\n{\"ok\":true}"
        )
    }

    // MARK: - Models, AXElement & SwipeV2 error classification

    func testAXElementDepthAndEncodingMatchClientExpectations() throws {
        let leaf = AXElement(
            identifier: "btn_allow",
            frame: ["X": 66, "Y": 474, "Width": 270, "Height": 44],
            label: "Allow While Using App",
            elementType: 9,
            enabled: true
        )
        let root = AXElement(
            frame: ["X": 0, "Y": 0, "Width": 402, "Height": 874],
            elementType: 0,
            enabled: true,
            children: [AXElement(elementType: 7, enabled: true, children: [leaf])]
        )
        XCTAssertEqual(leaf.depth(), 1)
        XCTAssertEqual(root.depth(), 3)

        let payload = try JSONEncoder().encode(ViewHierarchyResponse(axElement: root, depth: root.depth()))
        let decoded = try JSONDecoder().decode(ViewHierarchyResponse.self, from: payload)
        XCTAssertEqual(decoded.depth, 3)
        XCTAssertEqual(decoded.axElement.children?.first?.children?.first?.label, "Allow While Using App")
        XCTAssertEqual(decoded.axElement.children?.first?.children?.first?.frame["Width"], 270)
    }

    func testAppErrorEncodesBothErrorCodeAndCode() throws {
        let err = AppError(type: .timeout, message: "Timed out while evaluating UI query.")
        XCTAssertEqual(err.statusCode, 408)
        let obj = try JSONSerialization.jsonObject(with: err.httpResponse.body) as? [String: String]
        XCTAssertEqual(obj?["errorMessage"], "Timed out while evaluating UI query.")
        XCTAssertEqual(obj?["errorCode"], "timeout")
        XCTAssertEqual(obj?["code"], "timeout")
    }

    func testSwipeErrorClassificationMatchesMaestro() {
        let uiQueryTimeout = NSError(
            domain: "com.apple.dt.XCTest.XCTFuture",
            code: 1000,
            userInfo: [NSLocalizedDescriptionKey: "Timed out while evaluating UI query."]
        )
        XCTAssertEqual(classifySwipeError(uiQueryTimeout).type, .timeout)
        XCTAssertEqual(classifySwipeError(uiQueryTimeout).statusCode, 408)

        let mainThreadBusy = NSError(
            domain: "com.apple.dt.xctest.automation-support.error",
            code: 6,
            userInfo: [NSLocalizedDescriptionKey: "Unable to perform work on main run loop, process main thread busy for 5.0s"]
        )
        XCTAssertEqual(classifySwipeError(mainThreadBusy).type, .timeout)

        let wrongDomain = NSError(
            domain: "com.example.other",
            code: 1000,
            userInfo: [NSLocalizedDescriptionKey: "Timed out while evaluating UI query."]
        )
        XCTAssertEqual(classifySwipeError(wrongDomain).type, .internal)
        XCTAssertEqual(classifySwipeError(wrongDomain).statusCode, 500)

        let wrongMessage = NSError(
            domain: "com.apple.dt.XCTest.XCTFuture",
            code: 1000,
            userInfo: [NSLocalizedDescriptionKey: "Unrelated failure"]
        )
        XCTAssertEqual(classifySwipeError(wrongMessage).type, .internal)
    }

    // MARK: - Orientation math

    func testOrientationScreenSizeAndPointTransforms() {
        // Portrait dims reported as (402, 874)
        let landFromPortrait = OrientationMath.actualScreenSize(
            physicalWidth: 402,
            physicalHeight: 874,
            orientation: .landscapeLeft
        )
        XCTAssertEqual(landFromPortrait.width, 874)
        XCTAssertEqual(landFromPortrait.height, 402)

        // Already landscape-swapped dims (874, 402) must NOT be double-swapped.
        let landAlreadySwapped = OrientationMath.actualScreenSize(
            physicalWidth: 874,
            physicalHeight: 402,
            orientation: .landscapeRight
        )
        XCTAssertEqual(landAlreadySwapped.width, 874)
        XCTAssertEqual(landAlreadySwapped.height, 402)

        // Point transforms
        let p = LogicalPoint(x: 100, y: 50)
        XCTAssertEqual(
            OrientationMath.orientationAwarePoint(physicalWidth: 402, physicalHeight: 874, point: p, orientation: .portrait),
            LogicalPoint(x: 100, y: 50)
        )
        XCTAssertEqual(
            OrientationMath.orientationAwarePoint(physicalWidth: 402, physicalHeight: 874, point: p, orientation: .portraitUpsideDown),
            LogicalPoint(x: 302, y: 824)
        )
        XCTAssertEqual(
            OrientationMath.orientationAwarePoint(physicalWidth: 402, physicalHeight: 874, point: p, orientation: .landscapeLeft),
            LogicalPoint(x: 352, y: 100)
        )
        // Same result when snapshot already gave landscape dims (874, 402).
        XCTAssertEqual(
            OrientationMath.orientationAwarePoint(physicalWidth: 874, physicalHeight: 402, point: p, orientation: .landscapeLeft),
            LogicalPoint(x: 352, y: 100)
        )
        XCTAssertEqual(
            OrientationMath.orientationAwarePoint(physicalWidth: 402, physicalHeight: 874, point: p, orientation: .landscapeRight),
            LogicalPoint(x: 50, y: 774)
        )

        XCTAssertTrue(
            OrientationMath.isSameAreaDifferentOrientation(
                deviceWidth: 402,
                deviceHeight: 874,
                appWidth: 874,
                appHeight: 402
            )
        )
        XCTAssertFalse(
            OrientationMath.isSameAreaDifferentOrientation(
                deviceWidth: 402,
                deviceHeight: 874,
                appWidth: 300,
                appHeight: 600
            )
        )
    }
}
