import Foundation
import XCTest
@testable import TapwingRunnerCore

final class TypingPlanTests: XCTestCase {
    private func drain(_ plan: inout TypingPlan) throws -> [TypingStep] {
        var out: [TypingStep] = []
        while let step = try plan.next(shouldStop: false) {
            out.append(step)
        }
        return out
    }

    func testEmptyTextHasNoSteps() throws {
        var plan = TypingPlan(text: "")
        XCTAssertFalse(plan.hasMore)
        XCTAssertNil(try plan.next(shouldStop: false))
        XCTAssertNil(try plan.next(shouldStop: true), "nothing left to stop")
    }

    func testShortTextIsOneSlowCharacterAndOneEventAsBeforeChunking() throws {
        var single = TypingPlan(text: "a")
        XCTAssertEqual(try drain(&single), [TypingStep(text: "a", slow: true)])

        let text = String(repeating: "x", count: TypingPlan.slowPrefixCount + TypingPlan.chunkSize)
        var plan = TypingPlan(text: text)
        let steps = try drain(&plan)
        XCTAssertEqual(steps.count, 2)
        XCTAssertEqual(steps[0], TypingStep(text: "x", slow: true))
        XCTAssertEqual(steps[1].text.count, TypingPlan.chunkSize)
        XCTAssertFalse(steps[1].slow)
    }

    func testLongTextIsChunkedAndReassemblesExactly() throws {
        let text = (0..<1000).map { String($0 % 10) }.joined()
        var plan = TypingPlan(text: text)
        let steps = try drain(&plan)
        XCTAssertEqual(steps.count, 1 + 17, "1 slow character, then 999 characters in chunks of 60")
        XCTAssertEqual(steps.map(\.text).joined(), text)
        XCTAssertEqual(steps.filter(\.slow).count, 1)
        XCTAssertTrue(steps.dropFirst().allSatisfy { $0.text.count <= TypingPlan.chunkSize })
        XCTAssertEqual(plan.typedCharacters, 1000)
        XCTAssertEqual(plan.totalCharacters, 1000)
    }

    func testChunksNeverSplitGraphemeClusters() throws {
        // Family emoji (ZWJ sequence), a flag, a combining acute accent and Czech diacritics.
        let cluster = "👨‍👩‍👧‍👦🇨🇿e\u{301}ůň"
        let text = String(repeating: cluster, count: 7)
        var plan = TypingPlan(text: text, chunkSize: 3)
        let steps = try drain(&plan)
        XCTAssertEqual(steps.map(\.text).joined(), text)
        XCTAssertEqual(steps.map(\.text.count).reduce(0, +), text.count)
        XCTAssertEqual(steps[0].text, "👨‍👩‍👧‍👦")
        XCTAssertTrue(steps.dropFirst().dropLast().allSatisfy { $0.text.count == 3 })
    }

    func testTypingStopsBetweenChunksOnceTheRequestIsDead() throws {
        var plan = TypingPlan(text: String(repeating: "a", count: 200), chunkSize: 50)
        XCTAssertEqual(try plan.next(shouldStop: false)?.text.count, 1)
        XCTAssertEqual(try plan.next(shouldStop: false)?.text.count, 50)
        XCTAssertThrowsError(try plan.next(shouldStop: true)) { error in
            XCTAssertEqual(error as? TypingStopped, TypingStopped(typedCharacters: 51, totalCharacters: 200))
        }
        XCTAssertEqual(plan.typedCharacters, 51, "a refused step types nothing")
        XCTAssertTrue(plan.hasMore)
    }

    func testADeadRequestTypesNothingAtAll() {
        var plan = TypingPlan(text: "hello")
        XCTAssertThrowsError(try plan.next(shouldStop: true)) { error in
            let stopped = error as? TypingStopped
            XCTAssertEqual(stopped?.typedCharacters, 0)
            XCTAssertEqual(stopped?.appError.type, .deadlineExceeded)
            XCTAssertEqual(
                stopped?.appError.message,
                "Stopped typing after 0 of 5 characters: the request deadline passed or the client disconnected"
            )
        }
    }

    func testANonPositiveChunkSizeStillMakesProgress() throws {
        var plan = TypingPlan(text: "abc", chunkSize: 0)
        XCTAssertEqual(try drain(&plan).map(\.text), ["a", "b", "c"])
    }

    func testAGateTicketDrivesTheStopFlag() throws {
        final class Clock: @unchecked Sendable { var now: TimeInterval = 0 }
        let clock = Clock()
        let gate = RequestGate(now: { clock.now })
        guard case .admitted(let ticket) = gate.submit(label: "POST /inputText", budget: 10, start: { _ in }) else {
            return XCTFail("expected admission")
        }
        var plan = TypingPlan(text: String(repeating: "a", count: 500))
        XCTAssertNotNil(try plan.next(shouldStop: ticket.shouldStop))
        XCTAssertNotNil(try plan.next(shouldStop: ticket.shouldStop))
        clock.now = 10
        XCTAssertThrowsError(try plan.next(shouldStop: ticket.shouldStop))
        XCTAssertEqual(plan.typedCharacters, 61)
    }
}
