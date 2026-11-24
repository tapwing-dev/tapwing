import Foundation
import XCTest
@testable import TapwingRunnerCore

/// A hand-driven monotonic clock: the tests never sleep or use timers.
private final class FakeClock: @unchecked Sendable {
    private let lock = NSLock()
    private var value: TimeInterval = 1000

    var now: TimeInterval {
        lock.lock()
        defer { lock.unlock() }
        return value
    }

    func advance(_ seconds: TimeInterval) {
        lock.lock()
        value += seconds
        lock.unlock()
    }
}

/// Records which requests were started, in order.
private final class StartLog: @unchecked Sendable {
    private let lock = NSLock()
    private var labels: [String] = []

    func record(_ ticket: RequestTicket) {
        lock.lock()
        labels.append(ticket.label)
        lock.unlock()
    }

    var started: [String] {
        lock.lock()
        defer { lock.unlock() }
        return labels
    }
}

final class RequestGateTests: XCTestCase {
    private let clock = FakeClock()
    private let log = StartLog()

    private func makeGate() -> RequestGate {
        let clock = self.clock
        return RequestGate(now: { clock.now })
    }

    private func submit(_ gate: RequestGate, _ label: String, budget: TimeInterval = 25) -> RequestTicket? {
        let log = self.log
        guard case .admitted(let ticket) = gate.submit(label: label, budget: budget, start: { log.record($0) }) else {
            return nil
        }
        return ticket
    }

    // MARK: - Budget

    func testBudgetDefaultsBelowTheHostTimeoutWithoutAHeader() {
        XCTAssertEqual(RequestBudget.seconds(fromHeaders: [:]), 25)
        XCTAssertLessThan(RequestBudget.defaultSeconds, 30)
        for bad in ["", "abc", "0", "-5", "nan", "inf"] {
            XCTAssertEqual(RequestBudget.seconds(fromHeaders: [RequestBudget.headerName: bad]), 25, bad)
        }
    }

    func testBudgetLeavesAMarginUnderTheHostsRemainingTime() {
        let header = RequestBudget.headerName
        XCTAssertEqual(header, "x-tapwing-deadline-ms", "HttpFraming lowercases header names")
        XCTAssertEqual(RequestBudget.seconds(fromHeaders: [header: "30000"]), 28)
        XCTAssertEqual(RequestBudget.seconds(fromHeaders: [header: " 90000 "]), 88)
        // A tiny budget keeps half of it rather than going negative.
        XCTAssertEqual(RequestBudget.seconds(fromHeaders: [header: "300"]), 0.15, accuracy: 1e-9)
        XCTAssertEqual(RequestBudget.seconds(fromHeaders: [header: "99999999999"]), RequestBudget.maxSeconds)
    }

    func testTheBudgetHeaderSurvivesHttpParsing() throws {
        var buffer = Data("POST /touch HTTP/1.1\r\nX-Tapwing-Deadline-Ms: 12000\r\nContent-Length: 2\r\n\r\n{}".utf8)
        guard case .complete(let request) = HttpFraming.tryParseRequest(from: &buffer) else {
            return XCTFail("request did not parse")
        }
        XCTAssertEqual(RequestBudget.seconds(fromHeaders: request.headers), 10)
    }

    // MARK: - Serialization

    func testRequestsRunOneAtATimeInArrivalOrder() throws {
        let gate = makeGate()
        let first = try XCTUnwrap(submit(gate, "POST /touch"))
        let second = try XCTUnwrap(submit(gate, "POST /viewHierarchy"))
        let third = try XCTUnwrap(submit(gate, "GET /screenshot"))
        XCTAssertEqual(log.started, ["POST /touch"], "queued requests wait for the gate")
        XCTAssertEqual(gate.snapshot(), GateSnapshot(running: "POST /touch", queued: 2))

        XCTAssertTrue(gate.finish(first))
        XCTAssertEqual(log.started, ["POST /touch", "POST /viewHierarchy"])
        XCTAssertTrue(gate.finish(second))
        XCTAssertTrue(gate.finish(third))
        XCTAssertEqual(log.started, ["POST /touch", "POST /viewHierarchy", "GET /screenshot"])
        XCTAssertEqual(gate.snapshot(), GateSnapshot())
        XCTAssertFalse(gate.finish(third), "a second finish is ignored")
    }

    // MARK: - Deadline of a running handler

    func testAnOverdueHandlerIsAnsweredOnceAndKeepsTheGate() throws {
        let gate = makeGate()
        let wedged = try XCTUnwrap(submit(gate, "POST /viewHierarchy", budget: 25))
        XCTAssertFalse(wedged.shouldStop)

        clock.advance(25)
        XCTAssertTrue(wedged.shouldStop, "the handler is told to stop producing side effects")
        XCTAssertEqual(gate.expire(wedged), .inFlight)
        XCTAssertEqual(gate.expire(wedged), .alreadyAnswered, "the timeout answer is sent once")

        // The handler cannot be cancelled: it still holds the gate and the runner reports it.
        clock.advance(6)
        let state = gate.snapshot()
        XCTAssertEqual(state, GateSnapshot(running: "POST /viewHierarchy", runningSeconds: 31, wedged: true))
        let status = GateReply.status(state)
        XCTAssertEqual(status, StatusResponse(status: "wedged", busy: "POST /viewHierarchy", busyMs: 31000, wedged: true))

        // New requests are refused at once instead of queueing behind it.
        guard case .refused(let blockedBy) = gate.submit(label: "POST /touch", budget: 25, start: { _ in
            XCTFail("a refused request must never start")
        }) else {
            return XCTFail("expected a refusal while wedged")
        }
        XCTAssertEqual(blockedBy, state)
        XCTAssertEqual(gate.snapshot().queued, 0)

        // If the handler does return, its late response is dropped and the gate heals.
        XCTAssertFalse(gate.finish(wedged))
        XCTAssertEqual(gate.snapshot(), GateSnapshot())
        XCTAssertNotNil(submit(gate, "POST /touch"))
        XCTAssertEqual(log.started, ["POST /viewHierarchy", "POST /touch"])
    }

    func testAHandlerThatFinishesInTimeWinsOverItsWatchdog() throws {
        let gate = makeGate()
        let ticket = try XCTUnwrap(submit(gate, "POST /touch"))
        clock.advance(24.9)
        XCTAssertTrue(gate.finish(ticket))
        clock.advance(1)
        XCTAssertEqual(gate.expire(ticket), .alreadyAnswered)
        XCTAssertFalse(gate.snapshot().wedged)
    }

    func testABusyButNotOverdueHandlerStillQueuesRequests() throws {
        let gate = makeGate()
        let slow = try XCTUnwrap(submit(gate, "POST /viewHierarchy"))
        clock.advance(24)
        XCTAssertFalse(gate.snapshot().wedged)
        XCTAssertEqual(GateReply.status(gate.snapshot()).status, "ok")
        let next = try XCTUnwrap(submit(gate, "POST /touch"), "busy is not wedged")
        XCTAssertTrue(gate.finish(slow))
        XCTAssertEqual(log.started, ["POST /viewHierarchy", "POST /touch"])
        XCTAssertTrue(gate.finish(next))
    }

    // MARK: - Queued requests that died

    func testAQueuedRequestWhoseClientHungUpIsNeverStarted() throws {
        let gate = makeGate()
        let running = try XCTUnwrap(submit(gate, "POST /viewHierarchy"))
        let abandoned = try XCTUnwrap(submit(gate, "POST /touch"))
        let alive = try XCTUnwrap(submit(gate, "POST /pressKey"))

        gate.clientGone(abandoned)
        XCTAssertTrue(abandoned.shouldStop)
        XCTAssertEqual(gate.snapshot().queued, 1)

        XCTAssertTrue(gate.finish(running))
        XCTAssertEqual(log.started, ["POST /viewHierarchy", "POST /pressKey"], "the dead tap never runs")
        XCTAssertEqual(gate.expire(abandoned), .alreadyAnswered, "nobody is left to answer")
        XCTAssertTrue(gate.finish(alive))
    }

    func testAQueuedRequestThatTimesOutIsAnsweredAndNeverStarted() throws {
        let gate = makeGate()
        let running = try XCTUnwrap(submit(gate, "POST /viewHierarchy", budget: 60))
        let waiting = try XCTUnwrap(submit(gate, "POST /touch", budget: 10))

        clock.advance(10)
        guard case .neverStarted(let blockedBy) = gate.expire(waiting) else {
            return XCTFail("expected the queued request to expire unstarted")
        }
        XCTAssertEqual(blockedBy, GateSnapshot(running: "POST /viewHierarchy", runningSeconds: 10))
        XCTAssertEqual(gate.expire(waiting), .alreadyAnswered)

        XCTAssertTrue(gate.finish(running))
        XCTAssertEqual(log.started, ["POST /viewHierarchy"])
        XCTAssertEqual(gate.snapshot(), GateSnapshot())
    }

    func testAQueuedRequestPastItsDeadlineIsSkippedEvenBeforeItsWatchdogFires() throws {
        let gate = makeGate()
        let running = try XCTUnwrap(submit(gate, "POST /viewHierarchy", budget: 60))
        let stale = try XCTUnwrap(submit(gate, "POST /touch", budget: 10))
        let fresh = try XCTUnwrap(submit(gate, "POST /pressKey", budget: 60))

        clock.advance(11)
        // The handler returns before the watchdog of the stale request ran.
        XCTAssertTrue(gate.finish(running))
        XCTAssertEqual(log.started, ["POST /viewHierarchy", "POST /pressKey"])
        guard case .neverStarted = gate.expire(stale) else {
            return XCTFail("the skipped request is still owed a timeout answer")
        }
        XCTAssertTrue(gate.finish(fresh))
    }

    func testTheClientOfARunningRequestHangingUpStopsTheHandlerButKeepsTheGate() throws {
        let gate = makeGate()
        let running = try XCTUnwrap(submit(gate, "POST /inputText"))
        let queued = try XCTUnwrap(submit(gate, "POST /touch"))

        gate.clientGone(running)
        XCTAssertTrue(running.shouldStop)
        XCTAssertEqual(log.started, ["POST /inputText"], "the handler has not returned yet")
        XCTAssertEqual(gate.snapshot().running, "POST /inputText")

        XCTAssertFalse(gate.finish(running), "nobody reads the response")
        XCTAssertEqual(log.started, ["POST /inputText", "POST /touch"])
        XCTAssertTrue(gate.finish(queued))
    }

    // MARK: - Wire

    func testTimeoutAnswersUseDistinctErrorCodesTheHostCanClassify() throws {
        let gate = makeGate()
        let ticket = try XCTUnwrap(submit(gate, "POST /viewHierarchy", budget: 28))
        clock.advance(28)

        let inFlight = GateReply.inFlight(ticket)
        XCTAssertEqual(inFlight.type, .deadlineExceeded)
        XCTAssertEqual(inFlight.statusCode, 408)
        XCTAssertTrue(inFlight.message.contains("`POST /viewHierarchy` did not finish within 28.0 s"), inFlight.message)

        let refused = GateReply.refused(label: "POST /touch", blockedBy: gate.snapshot())
        XCTAssertEqual(refused.type, .runnerBusy)
        XCTAssertEqual(refused.statusCode, 408)
        XCTAssertTrue(refused.message.contains("the runner is wedged in `POST /viewHierarchy`"), refused.message)

        let waited = GateReply.neverStarted(ticket, blockedBy: GateSnapshot(running: "POST /swipeV2", runningSeconds: 3))
        XCTAssertEqual(waited.type, .runnerBusy)
        XCTAssertTrue(waited.message.contains("is still busy with `POST /swipeV2` (running for 3.0 s)"), waited.message)

        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: inFlight.httpResponse.body) as? [String: String])
        XCTAssertEqual(json["errorCode"], "deadlineExceeded")
        XCTAssertEqual(json["code"], "deadlineExceeded")
        XCTAssertEqual(AppErrorType.runnerBusy.rawValue, "runnerBusy")
    }

    func testAnIdleStatusStaysTheOriginalMinimalBody() throws {
        let body = HttpResponse.json(GateReply.status(GateSnapshot())).body
        XCTAssertEqual(try JSONSerialization.jsonObject(with: body) as? [String: String], ["status": "ok"])

        let busy = GateReply.status(GateSnapshot(running: "POST /touch", runningSeconds: 0.25))
        XCTAssertEqual(busy, StatusResponse(status: "ok", busy: "POST /touch", busyMs: 250))
    }
}
