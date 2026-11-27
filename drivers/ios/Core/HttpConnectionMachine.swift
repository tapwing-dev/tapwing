// Pure buffered HTTP/1.1 connection state machine shared between `HttpServer`
// (`ClientConnection`) and SwiftPM unit tests (`TapwingRunnerCore`).

import Foundation

public struct HttpConnectionMachine: Sendable {
    public enum ReadAction: Equatable, Sendable {
        case needMoreData
        case close
        case malformed(reason: String, wireResponse: Data)
        case statusFastPath(request: HttpRequest, keepAlive: Bool)
        case dispatch(request: HttpRequest, keepAlive: Bool)
        case ignoreWhileInFlight(continueReading: Bool)
    }

    public enum SendAction: Equatable, Sendable {
        case close
        case processNext
    }

    public private(set) var isClosed = false
    public private(set) var isReceiving = false
    public private(set) var sawEOF = false
    public private(set) var hasInFlight = false
    private var buffer = Data()
    private let maxBufferBytes: Int

    public init(maxBufferBytes: Int = HttpFraming.maxHeaderBytes + HttpFraming.maxBodyBytes) {
        self.maxBufferBytes = maxBufferBytes
    }

    public var bufferedByteCount: Int {
        buffer.count
    }

    /// Claims a read slot if the connection can still read from the socket.
    public mutating func beginReceive() -> Bool {
        guard !isClosed, !isReceiving, !sawEOF else { return false }
        isReceiving = true
        return true
    }

    /// Incorporates newly arrived bytes / EOF / socket error and returns the next action.
    public mutating func onReceive(data: Data?, isComplete: Bool, hadError: Bool) -> ReadAction {
        guard !isClosed else { return .close }
        isReceiving = false
        if let data, !data.isEmpty {
            buffer.append(data)
        }
        if hadError {
            return .close
        }
        if isComplete {
            sawEOF = true
        }
        if hasInFlight {
            // While a handler runs, reads only watch for peer hangup so a gone client's
            // queued request is never dispatched.
            if sawEOF {
                return .close
            }
            return .ignoreWhileInFlight(continueReading: buffer.count <= maxBufferBytes)
        }
        return pollNextRequest()
    }

    /// Parses the next request from `buffer` when idle and open.
    public mutating func pollNextRequest() -> ReadAction {
        guard !isClosed, !hasInFlight else {
            return .ignoreWhileInFlight(continueReading: false)
        }
        switch HttpFraming.tryParseRequest(from: &buffer) {
        case .incomplete:
            return sawEOF ? .close : .needMoreData
        case .malformed(let reason):
            let resp = AppError(type: .precondition, message: reason).httpResponse
            return .malformed(reason: reason, wireResponse: HttpFraming.serialize(resp, keepAlive: false))
        case .complete(let request):
            let keepAlive = request.shouldKeepAlive && !sawEOF
            if request.method == "GET" && request.path == "/status" {
                return .statusFastPath(request: request, keepAlive: keepAlive)
            }
            hasInFlight = true
            return .dispatch(request: request, keepAlive: keepAlive)
        }
    }

    /// Clears the in-flight flag when the admitted (or refused) request finishes replying.
    public mutating func completeInFlight() {
        hasInFlight = false
    }

    /// Handles completion of sending a response on the socket.
    public mutating func onSendFinished(hadError: Bool, keepAlive: Bool) -> SendAction {
        if isClosed || hadError || !keepAlive {
            return .close
        }
        return .processNext
    }

    /// Marks the connection closed once; returns `true` on the transition from open to closed.
    public mutating func markClosed() -> Bool {
        guard !isClosed else { return false }
        isClosed = true
        hasInFlight = false
        return true
    }
}
