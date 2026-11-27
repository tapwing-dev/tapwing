import Foundation
import XCTest
@testable import TapwingRunnerCore

final class HttpConnectionMachineTests: XCTestCase {

    func testFastPathStatusAndSequentialPipelinedKeepAliveRequests() {
        var m = HttpConnectionMachine()
        XCTAssertTrue(m.beginReceive())
        XCTAssertFalse(m.beginReceive(), "cannot start a second concurrent receive")

        let pipeline = Data(
            (
                "GET /status HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n" +
                "POST /touch HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 13\r\n\r\n{\"x\":1,\"y\":2}"
            ).utf8
        )
        let first = m.onReceive(data: pipeline, isComplete: false, hadError: false)
        guard case .statusFastPath(let statusReq, let statusKeepAlive) = first else {
            return XCTFail("expected /status fast path, got \(first)")
        }
        XCTAssertEqual(statusReq.path, "/status")
        XCTAssertTrue(statusKeepAlive)
        XCTAssertFalse(m.hasInFlight, "/status does not hold the gate")

        XCTAssertEqual(m.onSendFinished(hadError: false, keepAlive: statusKeepAlive), .processNext)
        let second = m.pollNextRequest()
        guard case .dispatch(let touchReq, let touchKeepAlive) = second else {
            return XCTFail("expected /touch dispatch, got \(second)")
        }
        XCTAssertEqual(touchReq.path, "/touch")
        XCTAssertTrue(touchKeepAlive)
        XCTAssertTrue(m.hasInFlight)

        m.completeInFlight()
        XCTAssertFalse(m.hasInFlight)
        XCTAssertEqual(m.onSendFinished(hadError: false, keepAlive: touchKeepAlive), .processNext)
        XCTAssertEqual(m.pollNextRequest(), .needMoreData)
    }

    func testClosesOnEofWhileInFlightAndRespectsBufferCap() {
        var m = HttpConnectionMachine(maxBufferBytes: 16)
        XCTAssertTrue(m.beginReceive())

        let req = Data("POST /hierarchy HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{}".utf8)
        guard case .dispatch(_, let keepAlive) = m.onReceive(data: req, isComplete: false, hadError: false) else {
            return XCTFail("expected dispatch")
        }
        XCTAssertTrue(keepAlive)
        XCTAssertTrue(m.hasInFlight)

        // Small queued tail while in flight exceeds maxBufferBytes -> pause reading without closing.
        XCTAssertTrue(m.beginReceive())
        XCTAssertEqual(
            m.onReceive(data: Data(repeating: UInt8(ascii: "x"), count: 17), isComplete: false, hadError: false),
            .ignoreWhileInFlight(continueReading: false)
        )

        // Peer hangup (EOF) while a gated request is in flight closes immediately.
        XCTAssertTrue(m.beginReceive())
        XCTAssertEqual(m.onReceive(data: nil, isComplete: true, hadError: false), .close)
        XCTAssertFalse(m.beginReceive(), "EOF ends receiving")
    }

    func testMalformedRequestReturnsWireErrorAndIncompleteEofCloses() {
        var incomplete = HttpConnectionMachine()
        XCTAssertTrue(incomplete.beginReceive())
        XCTAssertEqual(incomplete.onReceive(data: Data("GET /sta".utf8), isComplete: true, hadError: false), .close)

        var bad = HttpConnectionMachine()
        XCTAssertTrue(bad.beginReceive())
        let action = bad.onReceive(
            data: Data("BADREQUEST\r\n\r\n".utf8),
            isComplete: false,
            hadError: false
        )
        guard case .malformed(_, let wire) = action else {
            return XCTFail("expected malformed action, got \(action)")
        }
        let wireText = String(decoding: wire, as: UTF8.self)
        XCTAssertTrue(wireText.contains("400 Bad Request"))
        XCTAssertTrue(wireText.contains("Connection: close"))
    }

    func testCloseTransitionsAndSendErrorsStopKeepAlive() {
        var m = HttpConnectionMachine()
        XCTAssertEqual(m.onSendFinished(hadError: true, keepAlive: true), .close)
        XCTAssertEqual(m.onSendFinished(hadError: false, keepAlive: false), .close)

        let req = Data("GET /deviceInfo HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n".utf8)
        XCTAssertTrue(m.beginReceive())
        guard case .dispatch(_, let keepAlive) = m.onReceive(data: req, isComplete: false, hadError: false) else {
            return XCTFail("expected dispatch")
        }
        XCTAssertFalse(keepAlive)
        XCTAssertEqual(m.onSendFinished(hadError: false, keepAlive: keepAlive), .close)

        XCTAssertTrue(m.markClosed())
        XCTAssertFalse(m.markClosed(), "markClosed is idempotent")
        XCTAssertFalse(m.hasInFlight)
        XCTAssertFalse(m.beginReceive())
        XCTAssertEqual(m.onReceive(data: Data("x".utf8), isComplete: false, hadError: false), .close)
    }
}
