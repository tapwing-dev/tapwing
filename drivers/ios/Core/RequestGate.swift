// Per-request deadlines and the FIFO gate that serializes route handlers.
//
// XCUITest work runs on the main thread and cannot be cancelled: a private
// XCTest call that never returns keeps the main thread (or an un-resumed
// continuation) forever. The gate therefore never pretends to stop a handler.
// It only decides who still gets an answer:
//
// * every request carries a deadline below the host's own request timeout;
// * a watchdog on the server queue (never the main thread) answers a request
//   whose deadline passed, while its handler keeps the gate until it returns;
// * while such an overdue handler is still running the runner is "wedged":
//   new requests are refused at once and `/status` reports it, so the host can
//   restart the runner process, which is the only real recovery;
// * a queued request whose client hung up, or whose deadline passed while it
//   waited, is never started.
//
// Everything here is lock-based and clock-injected, so it is unit-tested on the
// host without a simulator, timers or sleeps.

import Foundation

// MARK: - Budget

public enum RequestBudget {
    /// Request header carrying the host's remaining time for this call, in milliseconds.
    public static let headerName = "x-tapwing-deadline-ms"
    /// Used when the host sends no (or an unreadable) budget: below its 30 s request timeout.
    public static let defaultSeconds: TimeInterval = 25
    /// Kept back from the host's budget so the timeout answer arrives before the host gives up.
    public static let replyMarginSeconds: TimeInterval = 2
    /// Upper bound on a host-supplied budget.
    public static let maxSeconds: TimeInterval = 600

    /// Seconds a handler may run before the runner answers with a timeout.
    public static func seconds(fromHeaders headers: [String: String]) -> TimeInterval {
        guard let raw = headers[headerName], let millis = Double(raw.trimmingCharacters(in: .whitespaces)),
              millis.isFinite, millis > 0 else {
            return defaultSeconds
        }
        let hostSeconds = millis / 1000
        let margin = min(replyMarginSeconds, hostSeconds / 2)
        return min(hostSeconds - margin, maxSeconds)
    }
}

// MARK: - Ticket

/// One admitted request. Handlers poll `shouldStop` between units of work
/// (for example between chunks of typed text).
public final class RequestTicket: @unchecked Sendable {
    fileprivate enum Phase {
        case queued
        case running
        /// Dropped from the queue because its deadline passed; the watchdog still owes the answer.
        case skipped
        case finished
    }

    /// `"POST /touch"`, for messages and `/status`.
    public let label: String
    public let budget: TimeInterval
    /// Monotonic time (the gate's clock) after which the request is dead.
    public let deadline: TimeInterval

    private let lock: NSLock
    private let now: @Sendable () -> TimeInterval
    // Guarded by `lock` (the gate's lock).
    fileprivate var phase: Phase = .queued
    fileprivate var replyClaimed = false
    fileprivate var clientGone = false
    fileprivate var startedAt: TimeInterval = 0
    fileprivate var start: (@Sendable (RequestTicket) -> Void)?

    fileprivate init(label: String, budget: TimeInterval, lock: NSLock, now: @escaping @Sendable () -> TimeInterval) {
        self.label = label
        self.budget = budget
        self.deadline = now() + budget
        self.lock = lock
        self.now = now
    }

    /// True once nobody waits for the result any more: the client hung up or
    /// the deadline passed. The handler should stop producing side effects.
    public var shouldStop: Bool {
        lock.lock()
        defer { lock.unlock() }
        return clientGone || now() >= deadline
    }
}

// MARK: - Gate

/// What the gate is doing, for `/status` and for refusal messages.
public struct GateSnapshot: Equatable, Sendable {
    /// Label of the handler holding the gate, if any.
    public let running: String?
    public let runningSeconds: TimeInterval
    /// The running handler is past its deadline and has not returned.
    public let wedged: Bool
    public let queued: Int

    public init(running: String? = nil, runningSeconds: TimeInterval = 0, wedged: Bool = false, queued: Int = 0) {
        self.running = running
        self.runningSeconds = runningSeconds
        self.wedged = wedged
        self.queued = queued
    }
}

public enum GateAdmission {
    /// The request holds the gate (`start` was called) or waits in the queue.
    case admitted(RequestTicket)
    /// The runner is wedged; the request was not queued and must be answered now.
    case refused(GateSnapshot)
}

public enum GateExpiry: Equatable, Sendable {
    /// The request was already answered (or its client is gone): nothing to send.
    case alreadyAnswered
    /// Timed out waiting in the queue; it never ran and never will.
    case neverStarted(GateSnapshot)
    /// Timed out while its handler runs; the handler keeps the gate.
    case inFlight
}

public final class RequestGate: @unchecked Sendable {
    private let lock = NSLock()
    private let now: @Sendable () -> TimeInterval
    private var running: RequestTicket?
    private var queue: [RequestTicket] = []

    public init(now: @escaping @Sendable () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }) {
        self.now = now
    }

    /// Admits a request. `start` runs exactly once when the request gets the
    /// gate: synchronously here when the gate is free, otherwise later from the
    /// `finish` of the request ahead. It never runs for a request that was
    /// refused, expired in the queue or lost its client.
    public func submit(
        label: String,
        budget: TimeInterval,
        start: @escaping @Sendable (RequestTicket) -> Void
    ) -> GateAdmission {
        lock.lock()
        if let busy = running, isOverdue(busy) {
            let snapshot = snapshotLocked()
            lock.unlock()
            return .refused(snapshot)
        }
        let ticket = RequestTicket(label: label, budget: budget, lock: lock, now: now)
        if running == nil {
            begin(ticket)
            lock.unlock()
            start(ticket)
        } else {
            ticket.start = start
            queue.append(ticket)
            lock.unlock()
        }
        return .admitted(ticket)
    }

    /// The handler of `ticket` returned. Returns whether its response should
    /// still be sent (false once the watchdog answered or the client left) and
    /// starts the next live queued request.
    public func finish(_ ticket: RequestTicket) -> Bool {
        lock.lock()
        guard ticket.phase == .running else {
            lock.unlock()
            return false
        }
        ticket.phase = .finished
        let deliver = !ticket.replyClaimed && !ticket.clientGone
        ticket.replyClaimed = true
        running = nil
        var next: (RequestTicket, @Sendable (RequestTicket) -> Void)?
        while !queue.isEmpty {
            let candidate = queue.removeFirst()
            let start = candidate.start
            candidate.start = nil
            if now() >= candidate.deadline {
                // Its watchdog is about to fire (or already did): leave the answer to it.
                candidate.phase = .skipped
                continue
            }
            guard let start else { continue }
            begin(candidate)
            next = (candidate, start)
            break
        }
        lock.unlock()
        if let (candidate, start) = next {
            start(candidate)
        }
        return deliver
    }

    /// The deadline of `ticket` passed (called by the server's watchdog timer).
    public func expire(_ ticket: RequestTicket) -> GateExpiry {
        lock.lock()
        defer { lock.unlock() }
        guard !ticket.replyClaimed else { return .alreadyAnswered }
        ticket.replyClaimed = true
        switch ticket.phase {
        case .running:
            return .inFlight
        case .queued:
            queue.removeAll { $0 === ticket }
            ticket.start = nil
            ticket.phase = .skipped
            return .neverStarted(snapshotLocked())
        case .skipped:
            return .neverStarted(snapshotLocked())
        case .finished:
            return .alreadyAnswered
        }
    }

    /// The connection of `ticket` closed. A queued request is dropped; a running
    /// handler sees `shouldStop` and keeps the gate until it returns.
    public func clientGone(_ ticket: RequestTicket) {
        lock.lock()
        defer { lock.unlock() }
        ticket.clientGone = true
        ticket.replyClaimed = true
        if ticket.phase == .queued {
            queue.removeAll { $0 === ticket }
            ticket.start = nil
            ticket.phase = .skipped
        }
    }

    public func snapshot() -> GateSnapshot {
        lock.lock()
        defer { lock.unlock() }
        return snapshotLocked()
    }

    private func begin(_ ticket: RequestTicket) {
        ticket.phase = .running
        ticket.startedAt = now()
        running = ticket
    }

    private func isOverdue(_ ticket: RequestTicket) -> Bool {
        now() >= ticket.deadline
    }

    private func snapshotLocked() -> GateSnapshot {
        guard let busy = running else {
            return GateSnapshot(queued: queue.count)
        }
        return GateSnapshot(
            running: busy.label,
            runningSeconds: max(0, now() - busy.startedAt),
            wedged: isOverdue(busy),
            queued: queue.count
        )
    }
}

// MARK: - Answers

public enum GateReply {
    /// The handler is still running past the deadline.
    public static func inFlight(_ ticket: RequestTicket) -> AppError {
        AppError(
            type: .deadlineExceeded,
            message: "`\(ticket.label)` did not finish within \(seconds(ticket.budget)); "
                + "the runner is still executing it and refuses other requests until it returns"
        )
    }

    /// The request waited in the queue until its deadline and was dropped.
    public static func neverStarted(_ ticket: RequestTicket, blockedBy gate: GateSnapshot) -> AppError {
        AppError(
            type: .runnerBusy,
            message: "`\(ticket.label)` was not started within \(seconds(ticket.budget)): \(describe(gate))"
        )
    }

    /// The request was refused because the runner is wedged.
    public static func refused(label: String, blockedBy gate: GateSnapshot) -> AppError {
        AppError(type: .runnerBusy, message: "`\(label)` was not started: \(describe(gate))")
    }

    /// `GET /status`: always HTTP 200 so readiness probes of older hosts keep
    /// working; the wedged state is in the body.
    public static func status(_ gate: GateSnapshot, session: String? = nil) -> StatusResponse {
        StatusResponse(
            status: gate.wedged ? "wedged" : "ok",
            busy: gate.running,
            busyMs: gate.running == nil ? nil : Int((gate.runningSeconds * 1000).rounded()),
            wedged: gate.wedged ? true : nil,
            session: session
        )
    }

    /// The session id from the runner's environment; nil when the host sent
    /// none (an older Tapwing) or an empty one.
    public static func session(fromEnvironment environment: [String: String]) -> String? {
        guard let raw = environment[sessionEnvironmentKey]?.trimmingCharacters(in: .whitespacesAndNewlines),
              !raw.isEmpty else {
            return nil
        }
        return raw
    }

    /// `xcodebuild` strips the `TEST_RUNNER_` prefix of `TEST_RUNNER_TAPWING_SESSION`.
    public static let sessionEnvironmentKey = "TAPWING_SESSION"

    private static func describe(_ gate: GateSnapshot) -> String {
        guard let running = gate.running else {
            return "the runner was busy"
        }
        let state = gate.wedged ? "is wedged in" : "is still busy with"
        return "the runner \(state) `\(running)` (running for \(seconds(gate.runningSeconds)))"
    }

    private static func seconds(_ value: TimeInterval) -> String {
        String(format: "%.1f s", value)
    }
}
