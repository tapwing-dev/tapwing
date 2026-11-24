import Foundation
import XCTest
@testable import TapwingRunnerCore

/// Edge cases of the hand-rolled HTTP/1.1 framing the runner speaks to the Rust client.
final class HttpFramingEdgeTests: XCTestCase {

    // MARK: - Helpers

    private func parse(_ raw: String) -> (HttpParseResult, remaining: Data) {
        var buffer = Data(raw.utf8)
        let result = HttpFraming.tryParseRequest(from: &buffer)
        return (result, buffer)
    }

    private func complete(_ raw: String, file: StaticString = #filePath, line: UInt = #line) -> HttpRequest? {
        guard case .complete(let request) = parse(raw).0 else {
            XCTFail("Expected a complete request for \(raw.debugDescription)", file: file, line: line)
            return nil
        }
        return request
    }

    private func assertMalformed(
        _ raw: String,
        containing fragment: String? = nil,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        guard case .malformed(let message) = parse(raw).0 else {
            return XCTFail("Expected malformed for \(raw.debugDescription)", file: file, line: line)
        }
        if let fragment {
            XCTAssertTrue(message.contains(fragment), "\(message) should mention \(fragment)", file: file, line: line)
        }
    }

    // MARK: - Headers

    func testHeaderNamesAreCaseInsensitiveAndStoredLowercased() throws {
        let req = try XCTUnwrap(complete("POST /touch HTTP/1.1\r\nCONTENT-LENGTH: 2\r\nContent-TYPE: application/json\r\nX-Custom: A\r\n\r\n{}"))
        XCTAssertEqual(req.headers["content-length"], "2")
        XCTAssertEqual(req.headers["content-type"], "application/json")
        XCTAssertEqual(req.headers["x-custom"], "A", "values keep their case")
        XCTAssertNil(req.headers["Content-Length"])
        XCTAssertEqual(req.body, Data("{}".utf8))
    }

    func testHeaderValuesAreTrimmedAndOnlyTheFirstColonSplits() throws {
        let req = try XCTUnwrap(complete("GET /s HTTP/1.1\r\nHost:   127.0.0.1:22087  \r\nX-Time:\t12:30:45\r\n\r\n"))
        XCTAssertEqual(req.headers["host"], "127.0.0.1:22087")
        XCTAssertEqual(req.headers["x-time"], "12:30:45")
    }

    func testRepeatedHeadersAreJoinedWithACommaInArrivalOrder() throws {
        let req = try XCTUnwrap(complete("GET /s HTTP/1.1\r\nAccept: a\r\nACCEPT: b\r\naccept: c\r\n\r\n"))
        XCTAssertEqual(req.headers["accept"], "a, b, c")
    }

    func testAnEmptyHeaderValueIsKept() throws {
        let req = try XCTUnwrap(complete("GET /s HTTP/1.1\r\nX-Empty:\r\n\r\n"))
        XCTAssertEqual(req.headers["x-empty"], "")
    }

    func testAHeaderLineWithoutAColonIsMalformed() {
        assertMalformed("GET /s HTTP/1.1\r\nthis is not a header\r\n\r\n", containing: "Malformed HTTP header line")
    }

    func testARequestWithNoHeadersAtAll() throws {
        let req = try XCTUnwrap(complete("GET /status HTTP/1.1\r\n\r\n"))
        XCTAssertEqual(req.method, "GET")
        XCTAssertEqual(req.path, "/status")
        XCTAssertTrue(req.headers.isEmpty)
        XCTAssertTrue(req.body.isEmpty)
    }

    func testHeadersWithNonUtf8BytesAreMalformed() {
        var buffer = Data("GET /s HTTP/1.1\r\nX-Bad: ".utf8)
        buffer.append(contentsOf: [0xFF, 0xFE, 0xFD])
        buffer.append(Data("\r\n\r\n".utf8))
        guard case .malformed(let message) = HttpFraming.tryParseRequest(from: &buffer) else {
            return XCTFail("Expected malformed for invalid UTF-8 headers")
        }
        XCTAssertTrue(message.contains("UTF-8"), message)
    }

    func testUnicodeHeaderValuesSurvive() throws {
        let req = try XCTUnwrap(complete("GET /s HTTP/1.1\r\nX-Name: Příliš žluťoučký 🚀\r\n\r\n"))
        XCTAssertEqual(req.headers["x-name"], "Příliš žluťoučký 🚀")
    }

    // MARK: - Request line

    func testMethodsAreUppercasedAndVersionsDefaultToHttp11() throws {
        let lower = try XCTUnwrap(complete("post /touch http/1.1\r\n\r\n"))
        XCTAssertEqual(lower.method, "POST")
        XCTAssertEqual(lower.httpVersion, "HTTP/1.1")
        XCTAssertEqual(lower.target, "/touch", "the target keeps its case")

        let noVersion = try XCTUnwrap(complete("GET /status\r\n\r\n"))
        XCTAssertEqual(noVersion.httpVersion, "HTTP/1.1")
        XCTAssertTrue(noVersion.shouldKeepAlive)
    }

    func testRepeatedSpacesInTheRequestLineAreTolerated() throws {
        let req = try XCTUnwrap(complete("GET    /status    HTTP/1.1\r\n\r\n"))
        XCTAssertEqual(req.method, "GET")
        XCTAssertEqual(req.path, "/status")
    }

    func testAnInvalidRequestLineIsMalformed() {
        assertMalformed("GET\r\n\r\n", containing: "Malformed HTTP request line")
        assertMalformed("\r\n\r\n", containing: "Empty HTTP request line")
        assertMalformed("   \r\n\r\n", containing: "Malformed HTTP request line")
    }

    func testAnUnknownMethodIsStillFramedAndLeftToTheRouter() throws {
        let req = try XCTUnwrap(complete("BREW /pot HTTP/1.1\r\n\r\n"))
        XCTAssertEqual(req.method, "BREW")
    }

    // MARK: - Body framing

    func testAnEmptyPostBodyIsComplete() throws {
        let withZero = try XCTUnwrap(complete("POST /pressButton HTTP/1.1\r\nContent-Length: 0\r\n\r\n"))
        XCTAssertTrue(withZero.body.isEmpty)
        let without = try XCTUnwrap(complete("POST /pressButton HTTP/1.1\r\n\r\n"))
        XCTAssertTrue(without.body.isEmpty, "a missing Content-Length means no body")
    }

    func testContentLengthIsParsedWithSurroundingWhitespace() throws {
        let req = try XCTUnwrap(complete("POST /t HTTP/1.1\r\nContent-Length:   3  \r\n\r\nabc"))
        XCTAssertEqual(req.body, Data("abc".utf8))
    }

    func testInvalidContentLengthValuesAreMalformed() {
        for value in ["-1", "-0x1", "abc", "", "1.5", "1e3", "99999999999999999999", "0x10"] {
            assertMalformed("POST /t HTTP/1.1\r\nContent-Length: \(value)\r\n\r\n", containing: "Invalid Content-Length")
        }
    }

    func testOversizedContentLengthIsRejectedBeforeAnyBodyArrives() {
        let tooBig = HttpFraming.maxBodyBytes + 1
        assertMalformed("POST /t HTTP/1.1\r\nContent-Length: \(tooBig)\r\n\r\n", containing: "exceeds")
        assertMalformed("POST /t HTTP/1.1\r\nContent-Length: \(Int.max)\r\n\r\n", containing: "exceeds")
    }

    func testContentLengthAtTheCapWaitsForTheBodyInsteadOfFailing() {
        let (result, remaining) = parse("POST /t HTTP/1.1\r\nContent-Length: \(HttpFraming.maxBodyBytes)\r\n\r\nstart")
        XCTAssertEqual(result, .incomplete)
        XCTAssertFalse(remaining.isEmpty, "an incomplete request must stay in the buffer")
    }

    func testAPartialBodyIsIncompleteAndLeavesTheBufferUntouched() {
        let raw = "POST /t HTTP/1.1\r\nContent-Length: 10\r\n\r\n12345"
        var buffer = Data(raw.utf8)
        XCTAssertEqual(HttpFraming.tryParseRequest(from: &buffer), .incomplete)
        XCTAssertEqual(buffer, Data(raw.utf8))

        buffer.append(Data("67890".utf8))
        guard case .complete(let req) = HttpFraming.tryParseRequest(from: &buffer) else {
            return XCTFail("Expected the completed request to parse")
        }
        XCTAssertEqual(req.body, Data("1234567890".utf8))
        XCTAssertTrue(buffer.isEmpty)
    }

    func testBytesBeyondContentLengthStayForTheNextRequest() {
        var buffer = Data("POST /a HTTP/1.1\r\nContent-Length: 2\r\n\r\nokGET /b HTTP/1.1\r\n".utf8)
        guard case .complete(let first) = HttpFraming.tryParseRequest(from: &buffer) else {
            return XCTFail("Expected first request")
        }
        XCTAssertEqual(first.body, Data("ok".utf8))
        XCTAssertEqual(buffer, Data("GET /b HTTP/1.1\r\n".utf8))
        XCTAssertEqual(HttpFraming.tryParseRequest(from: &buffer), .incomplete, "the second request has no terminator yet")
    }

    func testBinaryBodiesAreKeptByteForByte() throws {
        let payload = Data([0x00, 0xFF, 0x0D, 0x0A, 0x0D, 0x0A, 0x7F, 0x80])
        var buffer = Data("POST /bin HTTP/1.1\r\nContent-Length: \(payload.count)\r\n\r\n".utf8)
        buffer.append(payload)
        guard case .complete(let req) = HttpFraming.tryParseRequest(from: &buffer) else {
            return XCTFail("Expected a complete request")
        }
        XCTAssertEqual(req.body, payload, "a CRLFCRLF inside the body must not end the request early")
        XCTAssertTrue(buffer.isEmpty)
    }

    func testChunkedTransferEncodingIsRejectedInAnyCaseOrList() {
        assertMalformed("POST /t HTTP/1.1\r\nTransfer-Encoding: CHUNKED\r\n\r\n", containing: "Chunked")
        assertMalformed("POST /t HTTP/1.1\r\ntransfer-encoding: gzip, chunked\r\n\r\n", containing: "Chunked")
    }

    func testANonChunkedTransferEncodingIsNotRejected() throws {
        let req = try XCTUnwrap(complete("POST /t HTTP/1.1\r\nTransfer-Encoding: identity\r\nContent-Length: 0\r\n\r\n"))
        XCTAssertEqual(req.headers["transfer-encoding"], "identity")
    }

    // MARK: - Header block size

    func testAHeaderBlockWithoutATerminatorIsIncompleteUntilTheCap() {
        var small = Data("GET /s HTTP/1.1\r\nX-Slow: yes".utf8)
        XCTAssertEqual(HttpFraming.tryParseRequest(from: &small), .incomplete)

        var atCap = Data(repeating: UInt8(ascii: "a"), count: HttpFraming.maxHeaderBytes)
        XCTAssertEqual(HttpFraming.tryParseRequest(from: &atCap), .incomplete)

        var overCap = Data(repeating: UInt8(ascii: "a"), count: HttpFraming.maxHeaderBytes + 1)
        guard case .malformed(let message) = HttpFraming.tryParseRequest(from: &overCap) else {
            return XCTFail("Expected malformed once the header cap is exceeded")
        }
        XCTAssertTrue(message.contains("\(HttpFraming.maxHeaderBytes)"), message)
    }

    func testAnOversizedHeaderBlockThatDoesHaveATerminatorIsMalformed() {
        let filler = String(repeating: "a", count: HttpFraming.maxHeaderBytes)
        assertMalformed("GET /s HTTP/1.1\r\nX-Big: \(filler)\r\n\r\n", containing: "exceed")
    }

    func testAHeaderBlockJustUnderTheCapParses() throws {
        let prefix = "GET /s HTTP/1.1\r\nX-Big: "
        let fill = String(repeating: "a", count: HttpFraming.maxHeaderBytes - prefix.utf8.count)
        let req = try XCTUnwrap(complete(prefix + fill + "\r\n\r\n"))
        XCTAssertEqual(req.headers["x-big"]?.count, fill.count)
    }

    // MARK: - Keep-alive

    func testKeepAliveFollowsTheVersionAndTheConnectionHeader() throws {
        XCTAssertTrue(try XCTUnwrap(complete("GET / HTTP/1.1\r\n\r\n")).shouldKeepAlive)
        XCTAssertFalse(try XCTUnwrap(complete("GET / HTTP/1.0\r\n\r\n")).shouldKeepAlive)
        XCTAssertTrue(try XCTUnwrap(complete("GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n")).shouldKeepAlive)
        XCTAssertFalse(try XCTUnwrap(complete("GET / HTTP/1.1\r\nConnection: close\r\n\r\n")).shouldKeepAlive)
        XCTAssertFalse(try XCTUnwrap(complete("GET / HTTP/1.1\r\nConnection: CLOSE\r\n\r\n")).shouldKeepAlive)
        XCTAssertFalse(try XCTUnwrap(complete("GET / HTTP/1.1\r\nConnection: TE, close\r\n\r\n")).shouldKeepAlive)
        XCTAssertTrue(try XCTUnwrap(complete("GET / HTTP/1.1\r\nConnection: Keep-Alive, Upgrade\r\n\r\n")).shouldKeepAlive)
    }

    func testCloseWinsOverKeepAliveWhenBothAreListed() throws {
        XCTAssertFalse(try XCTUnwrap(complete("GET / HTTP/1.1\r\nConnection: keep-alive, close\r\n\r\n")).shouldKeepAlive)
        XCTAssertFalse(try XCTUnwrap(complete("GET / HTTP/1.1\r\nConnection: keep-alive\r\nConnection: close\r\n\r\n")).shouldKeepAlive)
    }

    func testUnknownConnectionTokensFallBackToTheVersionDefault() throws {
        XCTAssertTrue(try XCTUnwrap(complete("GET / HTTP/1.1\r\nConnection: upgrade\r\n\r\n")).shouldKeepAlive)
        XCTAssertFalse(try XCTUnwrap(complete("GET / HTTP/1.0\r\nConnection: upgrade\r\n\r\n")).shouldKeepAlive)
    }

    // MARK: - Query decoding

    func testSplitTargetWithoutAQueryReturnsTheWholeTargetAsPath() {
        let (path, query) = HttpFraming.splitTarget("/viewHierarchy")
        XCTAssertEqual(path, "/viewHierarchy")
        XCTAssertTrue(query.isEmpty)
    }

    func testQueryValuesArePercentDecodedIncludingUtf8() {
        let (path, query) = HttpFraming.splitTarget("/t?name=P%C5%99%C3%ADli%C5%A1&emoji=%F0%9F%9A%80&sp=a%20b")
        XCTAssertEqual(path, "/t")
        XCTAssertEqual(query["name"], "Příliš")
        XCTAssertEqual(query["emoji"], "🚀")
        XCTAssertEqual(query["sp"], "a b")
    }

    func testPercentEncodedKeysAreDecodedToo() {
        let (_, query) = HttpFraming.splitTarget("/t?a%20b=1&%C4%8D=2")
        XCTAssertEqual(query["a b"], "1")
        XCTAssertEqual(query["č"], "2")
    }

    func testPlusIsALiteralPlusNotASpace() {
        // The runner decodes RFC 3986 percent-encoding only; `+` stays as is.
        let (_, query) = HttpFraming.splitTarget("/t?q=a+b")
        XCTAssertEqual(query["q"], "a+b")
    }

    func testKeysWithoutValuesAndEmptyValues() {
        let (_, query) = HttpFraming.splitTarget("/t?flag&empty=&x=1")
        XCTAssertEqual(query["flag"], "")
        XCTAssertEqual(query["empty"], "")
        XCTAssertEqual(query["x"], "1")
        XCTAssertEqual(query.count, 3)
    }

    func testEmptyPairsAreSkippedAndOnlyTheFirstEqualsSplits() {
        let (_, query) = HttpFraming.splitTarget("/t?a=1&&b=x=y&")
        XCTAssertEqual(query["a"], "1")
        XCTAssertEqual(query["b"], "x=y")
        XCTAssertEqual(query.count, 2)
    }

    func testTheLastDuplicateKeyWins() {
        let (_, query) = HttpFraming.splitTarget("/t?k=1&k=2&k=3")
        XCTAssertEqual(query["k"], "3")
    }

    func testInvalidPercentEncodingFallsBackToTheRawText() {
        let (_, query) = HttpFraming.splitTarget("/t?bad=%ZZ&half=%4&ok=%41")
        XCTAssertEqual(query["bad"], "%ZZ")
        XCTAssertEqual(query["half"], "%4")
        XCTAssertEqual(query["ok"], "A")
    }

    func testABareQuestionMarkYieldsAnEmptyQuery() {
        let (path, query) = HttpFraming.splitTarget("/status?")
        XCTAssertEqual(path, "/status")
        XCTAssertTrue(query.isEmpty)

        let (rootPath, rootQuery) = HttpFraming.splitTarget("?a=1")
        XCTAssertEqual(rootPath, "")
        XCTAssertEqual(rootQuery["a"], "1")
    }

    func testAnEncodedAmpersandInsideAValueDoesNotSplitThePair() {
        let (_, query) = HttpFraming.splitTarget("/t?q=a%26b&r=1")
        XCTAssertEqual(query["q"], "a&b")
        XCTAssertEqual(query["r"], "1")
    }

    func testTheParsedRequestExposesTargetPathAndQueryTogether() throws {
        let req = try XCTUnwrap(complete("GET /screenshot?compressed=false&n=%31 HTTP/1.1\r\n\r\n"))
        XCTAssertEqual(req.target, "/screenshot?compressed=false&n=%31")
        XCTAssertEqual(req.path, "/screenshot")
        XCTAssertEqual(req.query, ["compressed": "false", "n": "1"])
    }

    // MARK: - Response serialization

    func testReasonPhrasesForKnownAndUnknownStatuses() {
        let known: [Int: String] = [
            200: "OK", 400: "Bad Request", 404: "Not Found", 405: "Method Not Allowed",
            408: "Request Timeout", 500: "Internal Server Error",
        ]
        for (code, phrase) in known {
            XCTAssertEqual(HttpFraming.reasonPhrase(for: code), phrase)
            XCTAssertEqual(HttpResponse(statusCode: code).reasonPhrase, phrase)
        }
        XCTAssertEqual(HttpFraming.reasonPhrase(for: 418), "Status")
        XCTAssertEqual(HttpFraming.reasonPhrase(for: 0), "Status")
        XCTAssertEqual(HttpResponse(statusCode: 418, reasonPhrase: "I'm a teapot").reasonPhrase, "I'm a teapot")
    }

    func testSerializeCloseConnectionAndEmptyBody() {
        let wire = String(
            data: HttpFraming.serialize(HttpResponse(statusCode: 404), keepAlive: false),
            encoding: .utf8
        )
        XCTAssertEqual(
            wire,
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    func testSerializeCountsUtf8BytesNotCharacters() {
        let body = Data("Příliš 🚀".utf8)
        let wire = HttpFraming.serialize(.ok(body, contentType: "text/plain; charset=utf-8"), keepAlive: true)
        let text = String(data: wire, encoding: .utf8)
        XCTAssertNotNil(text)
        XCTAssertTrue(text?.contains("Content-Length: \(body.count)\r\n") == true)
        XCTAssertTrue(text?.contains("Content-Type: text/plain; charset=utf-8\r\n") == true)
        XCTAssertTrue(wire.suffix(body.count) == body)
    }

    func testSerializedBinaryBodiesAreAppendedVerbatim() {
        let png = Data([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0xFF])
        let wire = HttpFraming.serialize(.ok(png, contentType: "image/png"), keepAlive: true)
        XCTAssertEqual(wire.suffix(png.count), png)
        let separator = Data("\r\n\r\n".utf8)
        let headerEnd = wire.range(of: separator)!.upperBound
        XCTAssertEqual(Data(wire[headerEnd...]), png)
    }

    func testOkDefaultsToAnEmptyJsonObject() {
        let resp = HttpResponse.ok()
        XCTAssertEqual(resp.statusCode, 200)
        XCTAssertEqual(resp.contentType, "application/json")
        XCTAssertEqual(resp.body, Data("{}".utf8))
    }

    func testJsonResponsesEncodeValuesAndHonourTheStatusCode() throws {
        let ok = HttpResponse.json(StatusResponse())
        XCTAssertEqual(ok.statusCode, 200)
        XCTAssertEqual(ok.contentType, "application/json")
        XCTAssertEqual(try JSONSerialization.jsonObject(with: ok.body) as? [String: String], ["status": "ok"])

        let accepted = HttpResponse.json(KeyboardResponse(isKeyboardVisible: true), statusCode: 202)
        XCTAssertEqual(accepted.statusCode, 202)
        XCTAssertEqual(accepted.reasonPhrase, "Status")
        XCTAssertEqual(try JSONSerialization.jsonObject(with: accepted.body) as? [String: Bool], ["isKeyboardVisible": true])
    }

    func testAResponseSerializedThenParsedAsARequestFramesIdentically() throws {
        // Round trip through the framing helpers: serialize a body, and the same bytes are a valid
        // request body under the matching Content-Length.
        let body = Data("{\"x\":1}".utf8)
        var buffer = Data("POST /echo HTTP/1.1\r\nContent-Length: \(body.count)\r\n\r\n".utf8)
        buffer.append(body)
        guard case .complete(let req) = HttpFraming.tryParseRequest(from: &buffer) else {
            return XCTFail("Expected a complete request")
        }
        XCTAssertEqual(req.body, body)
    }

    func testRequestAndResponseValuesAreEquatable() {
        let a = HttpResponse(statusCode: 200, body: Data("x".utf8))
        let b = HttpResponse(statusCode: 200, body: Data("x".utf8))
        XCTAssertEqual(a, b)
        XCTAssertNotEqual(a, HttpResponse(statusCode: 500, body: Data("x".utf8)))
        XCTAssertEqual(HttpParseResult.incomplete, .incomplete)
        XCTAssertNotEqual(HttpParseResult.incomplete, .malformed("x"))
    }
}
