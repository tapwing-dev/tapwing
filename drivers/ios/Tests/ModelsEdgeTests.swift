import Foundation
import XCTest
@testable import TapwingRunnerCore

/// Wire models, error serialization and the AXElement tree the Rust client decodes.
final class ModelsEdgeTests: XCTestCase {

    private func jsonObject(_ data: Data) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    // MARK: - AppError

    func testEveryErrorTypeMapsToItsHttpStatus() {
        XCTAssertEqual(AppError(type: .internal, message: "x").statusCode, 500)
        XCTAssertEqual(AppError(type: .precondition, message: "x").statusCode, 400)
        XCTAssertEqual(AppError(type: .timeout, message: "x").statusCode, 408)
        XCTAssertEqual(AppError(type: .notFound, message: "x").statusCode, 404)
        XCTAssertEqual(AppError(message: "default type").type, .internal)
    }

    func testErrorTypeRawValuesAreTheWireNames() {
        XCTAssertEqual(AppErrorType.internal.rawValue, "internal")
        XCTAssertEqual(AppErrorType.precondition.rawValue, "precondition")
        XCTAssertEqual(AppErrorType.timeout.rawValue, "timeout")
        XCTAssertEqual(AppErrorType.notFound.rawValue, "notFound")
    }

    func testErrorHttpResponseCarriesStatusReasonAndJsonBody() throws {
        for (type, status, reason) in [
            (AppErrorType.precondition, 400, "Bad Request"),
            (.notFound, 404, "Not Found"),
            (.timeout, 408, "Request Timeout"),
            (.internal, 500, "Internal Server Error"),
        ] {
            let response = AppError(type: type, message: "m").httpResponse
            XCTAssertEqual(response.statusCode, status)
            XCTAssertEqual(response.reasonPhrase, reason)
            XCTAssertEqual(response.contentType, "application/json")
            let obj = try jsonObject(response.body)
            XCTAssertEqual(obj["errorCode"] as? String, type.rawValue)
            XCTAssertEqual(obj["code"] as? String, type.rawValue)
            XCTAssertEqual(obj["errorMessage"] as? String, "m")
            XCTAssertEqual(obj.count, 3, "no other keys are emitted")
        }
    }

    func testErrorMessagesWithQuotesNewlinesAndUnicodeRoundTrip() throws {
        let message = "Element \"Přihlášení\" not found\n\t🚀 \\ / <&>"
        let original = AppError(type: .notFound, message: message)
        let decoded = try JSONDecoder().decode(AppError.self, from: original.httpResponse.body)
        XCTAssertEqual(decoded, original)
        XCTAssertEqual(decoded.message, message)
    }

    func testDecodingFallsBackFromErrorCodeToCodeToInternal() throws {
        func decode(_ json: String) throws -> AppError {
            try JSONDecoder().decode(AppError.self, from: Data(json.utf8))
        }
        XCTAssertEqual(try decode(#"{"errorMessage":"m","errorCode":"timeout"}"#).type, .timeout)
        XCTAssertEqual(try decode(#"{"errorMessage":"m","code":"notFound"}"#).type, .notFound)
        XCTAssertEqual(try decode(#"{"errorMessage":"m","errorCode":"precondition","code":"timeout"}"#).type, .precondition,
                       "errorCode (Tapwing) wins over code (Maestro)")
        XCTAssertEqual(try decode(#"{"errorMessage":"m"}"#).type, .internal)
        XCTAssertEqual(try decode(#"{"errorMessage":"m","errorCode":"somethingNew"}"#).type, .internal,
                       "an unknown code from a newer runner degrades to internal")
    }

    func testDecodingAnErrorWithoutAMessageFails() {
        XCTAssertThrowsError(try JSONDecoder().decode(AppError.self, from: Data(#"{"errorCode":"timeout"}"#.utf8)))
    }

    // MARK: - XCUITest timeout classification

    func testTimeoutClassificationNeedsBothDomainCodeAndMessage() {
        func error(_ domain: String, _ code: Int, _ message: String) -> NSError {
            NSError(domain: domain, code: code, userInfo: [NSLocalizedDescriptionKey: message])
        }
        let query = "Timed out while evaluating UI query."
        let busy = "Unable to perform work on main run loop, process main thread busy for 12.5s"

        XCTAssertTrue(error("com.apple.dt.XCTest.XCTFuture", 1000, query).isXCUITestTimeout)
        XCTAssertTrue(error("com.apple.dt.xctest.automation-support.error", 6, busy).isXCUITestTimeout)
        // Message embedded in a longer description still matches.
        XCTAssertTrue(error("com.apple.dt.XCTest.XCTFuture", 1000, "Failure: \(query) Retry.").isXCUITestTimeout)

        XCTAssertFalse(error("com.apple.dt.XCTest.XCTFuture", 1001, query).isXCUITestTimeout, "wrong code")
        XCTAssertFalse(error("com.apple.dt.xctest.automation-support.error", 7, busy).isXCUITestTimeout, "wrong code")
        XCTAssertFalse(error("com.apple.dt.XCTest.XCTFuture", 1000, busy).isXCUITestTimeout, "message of the other kind")
        XCTAssertFalse(error("com.apple.dt.xctest.automation-support.error", 6, query).isXCUITestTimeout, "message of the other kind")
        XCTAssertFalse(error("COM.APPLE.DT.XCTEST.XCTFUTURE", 1000, query).isXCUITestTimeout, "domains are case-sensitive")
        XCTAssertFalse(error("", 0, "").isXCUITestTimeout)
    }

    func testSwipeClassificationKeepsTheOriginalErrorTextInItsMessage() {
        struct Boom: LocalizedError { var errorDescription: String? { "boom: Příliš 🚀" } }
        let classified = classifySwipeError(Boom())
        XCTAssertEqual(classified.type, .internal)
        XCTAssertEqual(classified.statusCode, 500)
        XCTAssertEqual(classified.message, "Swipe v2 request failure. Error: boom: Příliš 🚀")

        let timeout = NSError(
            domain: "com.apple.dt.XCTest.XCTFuture",
            code: 1000,
            userInfo: [NSLocalizedDescriptionKey: "Timed out while evaluating UI query."]
        )
        XCTAssertTrue(classifySwipeError(timeout).message.hasPrefix("Swipe v2 request failure. Error: "))
        XCTAssertEqual(classifySwipeError(timeout).statusCode, 408)
    }

    // MARK: - AXFrame & AXElement

    func testAXFrameAccessorsDefaultToZeroForMissingKeys() {
        let full: AXFrame = ["X": 1.5, "Y": 2.5, "Width": 3.5, "Height": 4.5]
        XCTAssertEqual([full.x, full.y, full.width, full.height], [1.5, 2.5, 3.5, 4.5])

        let partial: AXFrame = ["Width": 10]
        XCTAssertEqual([partial.x, partial.y, partial.width, partial.height], [0, 0, 10, 0])

        let empty: AXFrame = [:]
        XCTAssertEqual([empty.x, empty.y, empty.width, empty.height], [0, 0, 0, 0])
        XCTAssertEqual(AXFrame.zero, ["X": 0, "Y": 0, "Width": 0, "Height": 0])
        // Lower-case keys are not the wire names.
        let lower: AXFrame = ["x": 9]
        XCTAssertEqual(lower.x, 0)
    }

    func testAXElementDefaults() {
        let e = AXElement()
        XCTAssertEqual(e.identifier, "")
        XCTAssertEqual(e.label, "")
        XCTAssertEqual(e.frame, .zero)
        XCTAssertNil(e.value)
        XCTAssertNil(e.title)
        XCTAssertNil(e.placeholderValue)
        XCTAssertNil(e.children)
        XCTAssertFalse(e.enabled)
        XCTAssertFalse(e.selected)
        XCTAssertFalse(e.hasFocus)
        XCTAssertEqual(e.elementType, 0)
        XCTAssertEqual(e.displayID, 0)
        XCTAssertEqual(e.windowContextID, 0)
    }

    func testDepthOfLeavesEmptyChildrenAndUnbalancedTrees() {
        XCTAssertEqual(AXElement().depth(), 1)
        XCTAssertEqual(AXElement(children: []).depth(), 1, "an empty children array is still a leaf")

        let deep = AXElement(children: [AXElement(children: [AXElement(children: [AXElement()])])])
        let shallow = AXElement(children: [AXElement(), AXElement(), AXElement()])
        XCTAssertEqual(deep.depth(), 4)
        XCTAssertEqual(shallow.depth(), 2)
        XCTAssertEqual(AXElement(children: [shallow, deep, AXElement()]).depth(), 5, "the deepest branch decides")
    }

    func testDepthOfAVeryDeepChainAndAWideTree() {
        var node = AXElement()
        for _ in 0..<500 { node = AXElement(children: [node]) }
        XCTAssertEqual(node.depth(), 501)

        let wide = AXElement(children: (0..<2000).map { AXElement(identifier: "child_\($0)") })
        XCTAssertEqual(wide.depth(), 2)
        XCTAssertEqual(wide.children?.count, 2000)
    }

    func testAXElementEncodesEveryClientFieldAndOmitsNils() throws {
        let element = AXElement(
            identifier: "login_button",
            frame: ["X": 1, "Y": 2, "Width": 3, "Height": 4],
            value: "v",
            title: "t",
            label: "Log in",
            elementType: 9,
            enabled: true,
            horizontalSizeClass: 1,
            verticalSizeClass: 2,
            placeholderValue: "p",
            selected: true,
            hasFocus: true,
            displayID: 7,
            windowContextID: 3.5
        )
        let obj = try jsonObject(try JSONEncoder().encode(element))
        XCTAssertEqual(
            Set(obj.keys),
            [
                "identifier", "frame", "value", "title", "label", "elementType", "enabled",
                "horizontalSizeClass", "verticalSizeClass", "placeholderValue", "selected",
                "hasFocus", "displayID", "windowContextID",
            ]
        )
        XCTAssertEqual(obj["frame"] as? [String: Double], ["X": 1, "Y": 2, "Width": 3, "Height": 4])
        XCTAssertEqual(obj["windowContextID"] as? Double, 3.5)

        let bare = try jsonObject(try JSONEncoder().encode(AXElement()))
        for omitted in ["value", "title", "placeholderValue", "children"] {
            XCTAssertNil(bare[omitted], "\(omitted) must be omitted when nil")
        }
    }

    func testAXElementRoundTripsUnicodeLabelsAndNestedChildren() throws {
        let tree = AXElement(
            identifier: "root",
            label: "Přihlášení 🚀 \"quoted\"",
            enabled: true,
            children: [
                AXElement(value: "úpěl", label: "Žluťoučký kůň", placeholderValue: "ďábelské"),
                AXElement(children: [AXElement(label: "日本語")]),
            ]
        )
        let decoded = try JSONDecoder().decode(AXElement.self, from: try JSONEncoder().encode(tree))
        XCTAssertEqual(decoded, tree)
        XCTAssertEqual(decoded.depth(), 3)
        XCTAssertEqual(decoded.children?[1].children?[0].label, "日本語")
    }

    func testDecodingAnAXElementWithMissingRequiredFieldsFails() {
        XCTAssertThrowsError(try JSONDecoder().decode(AXElement.self, from: Data("{}".utf8)))
    }

    func testViewHierarchyResponseCarriesTheDepth() throws {
        let root = AXElement(children: [AXElement(children: [AXElement()])])
        let response = ViewHierarchyResponse(axElement: root, depth: root.depth())
        let obj = try jsonObject(try JSONEncoder().encode(response))
        XCTAssertEqual(obj["depth"] as? Int, 3)
        XCTAssertNotNil(obj["axElement"] as? [String: Any])
    }

    // MARK: - Request DTOs

    func testRequestsDecodeWithTheirOptionalFieldsOmitted() throws {
        let decoder = JSONDecoder()
        let touch = try decoder.decode(TouchRequest.self, from: Data(#"{"x":10.5,"y":20}"#.utf8))
        XCTAssertEqual(touch, TouchRequest(x: 10.5, y: 20, duration: nil))

        let held = try decoder.decode(TouchRequest.self, from: Data(#"{"x":1,"y":2,"duration":1.5}"#.utf8))
        XCTAssertEqual(held.duration, 1.5)

        let hierarchy = try decoder.decode(ViewHierarchyRequest.self, from: Data("{}".utf8))
        XCTAssertNil(hierarchy.appIds)
        XCTAssertNil(hierarchy.excludeKeyboardElements)

        let swipe = try decoder.decode(
            SwipeV2Request.self,
            from: Data(#"{"startX":1,"startY":2,"endX":3,"endY":4,"duration":0.5}"#.utf8)
        )
        XCTAssertEqual(swipe, SwipeV2Request(startX: 1, startY: 2, endX: 3, endY: 4, duration: 0.5))
        XCTAssertNil(swipe.appId)
        XCTAssertNil(swipe.appIds)

        let erase = try decoder.decode(EraseTextRequest.self, from: Data(#"{"charactersToErase":5}"#.utf8))
        XCTAssertEqual(erase.charactersToErase, 5)
        XCTAssertNil(erase.appIds)
    }

    func testRequestsRejectMissingRequiredFieldsAndWrongTypes() {
        let decoder = JSONDecoder()
        XCTAssertThrowsError(try decoder.decode(TouchRequest.self, from: Data(#"{"x":1}"#.utf8)))
        XCTAssertThrowsError(try decoder.decode(TouchRequest.self, from: Data(#"{"x":"1","y":2}"#.utf8)))
        XCTAssertThrowsError(try decoder.decode(InputTextRequest.self, from: Data("{}".utf8)))
        XCTAssertThrowsError(try decoder.decode(EraseTextRequest.self, from: Data(#"{"charactersToErase":"many"}"#.utf8)))
        XCTAssertThrowsError(try decoder.decode(SwipeV2Request.self, from: Data(#"{"startX":1}"#.utf8)))
        XCTAssertThrowsError(try decoder.decode(RunningAppRequest.self, from: Data("{}".utf8)))
        XCTAssertThrowsError(try decoder.decode(KeyboardRequest.self, from: Data(#"{"appIds":"x"}"#.utf8)))
    }

    func testInputTextRequestKeepsUnicodeAndEscapesExactly() throws {
        let text = "Příliš žluťoučký kůň 🚀\n\t\"q\" \\ /"
        let data = try JSONEncoder().encode(InputTextRequest(text: text, appIds: ["dev.tapwing.sampleApp"]))
        let decoded = try JSONDecoder().decode(InputTextRequest.self, from: data)
        XCTAssertEqual(decoded.text, text)
        XCTAssertEqual(decoded.appIds, ["dev.tapwing.sampleApp"])
        // The runner also accepts text that was escaped by another JSON encoder.
        let escaped = try JSONDecoder().decode(InputTextRequest.self, from: Data(#"{"text":"č🚀"}"#.utf8))
        XCTAssertEqual(escaped.text, "č🚀")
    }

    func testEnumRequestsAcceptEveryDocumentedValueAndRejectOthers() throws {
        let decoder = JSONDecoder()
        for key in ["delete", "return", "enter", "tab", "space", "escape"] {
            let req = try decoder.decode(PressKeyRequest.self, from: Data(#"{"key":"\#(key)"}"#.utf8))
            XCTAssertEqual(req.key.rawValue, key)
        }
        for button in ["home", "lock"] {
            let req = try decoder.decode(PressButtonRequest.self, from: Data(#"{"button":"\#(button)"}"#.utf8))
            XCTAssertEqual(req.button.rawValue, button)
        }
        for orientation in ["portrait", "landscapeLeft", "landscapeRight", "upsideDown"] {
            let req = try decoder.decode(SetOrientationRequest.self, from: Data(#"{"orientation":"\#(orientation)"}"#.utf8))
            XCTAssertEqual(req.orientation.rawValue, orientation)
        }
        XCTAssertThrowsError(try decoder.decode(PressKeyRequest.self, from: Data(#"{"key":"F13"}"#.utf8)))
        XCTAssertThrowsError(try decoder.decode(PressKeyRequest.self, from: Data(#"{"key":"DELETE"}"#.utf8)), "case-sensitive")
        XCTAssertThrowsError(try decoder.decode(PressButtonRequest.self, from: Data(#"{"button":"power"}"#.utf8)))
        XCTAssertThrowsError(try decoder.decode(SetOrientationRequest.self, from: Data(#"{"orientation":"sideways"}"#.utf8)))
    }

    // MARK: - Response DTOs

    func testResponseDtosEncodeTheKeysTheClientReads() throws {
        XCTAssertEqual(StatusResponse().status, "ok")
        XCTAssertEqual(try jsonObject(try JSONEncoder().encode(StatusResponse(status: "busy")))["status"] as? String, "busy")

        let info = DeviceInfoResponse(widthPoints: 402, heightPoints: 874, widthPixels: 1206, heightPixels: 2622)
        let obj = try jsonObject(try JSONEncoder().encode(info))
        XCTAssertEqual(obj["widthPoints"] as? Int, 402)
        XCTAssertEqual(obj["heightPoints"] as? Int, 874)
        XCTAssertEqual(obj["widthPixels"] as? Int, 1206)
        XCTAssertEqual(obj["heightPixels"] as? Int, 2622)

        XCTAssertEqual(
            try jsonObject(try JSONEncoder().encode(RunningAppResponse(runningAppBundleId: "dev.tapwing.sampleApp")))["runningAppBundleId"] as? String,
            "dev.tapwing.sampleApp"
        )
        XCTAssertEqual(
            try jsonObject(try JSONEncoder().encode(ScreenStaticResponse(isScreenStatic: false)))["isScreenStatic"] as? Bool,
            false
        )
        XCTAssertEqual(
            try jsonObject(try JSONEncoder().encode(KeyboardResponse(isKeyboardVisible: true)))["isKeyboardVisible"] as? Bool,
            true
        )
    }

    func testResponseDtosRoundTrip() throws {
        let info = DeviceInfoResponse(widthPoints: 1, heightPoints: 2, widthPixels: 3, heightPixels: 4)
        XCTAssertEqual(try JSONDecoder().decode(DeviceInfoResponse.self, from: try JSONEncoder().encode(info)), info)
        let running = RunningAppResponse(runningAppBundleId: "x.y")
        XCTAssertEqual(try JSONDecoder().decode(RunningAppResponse.self, from: try JSONEncoder().encode(running)), running)
    }
}
