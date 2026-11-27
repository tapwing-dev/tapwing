// Zero-dependency HTTP/1.1 keep-alive loopback server using `Network.framework`.
// Binds to `127.0.0.1:<port>` with `TCP_NODELAY` enabled so persistent
// `ureq::Agent` connections from Tapwing incur zero Nagle/delayed-ACK latency.
//
// Threading: everything in this file runs on the serial server `queue`, never
// on the main thread. Route handlers run on the main actor, one at a time,
// behind `RequestGate` (see `Core/RequestGate.swift`); `GET /status` and the
// per-request deadline answers come from the server queue, so the runner keeps
// answering while a handler is wedged in an XCTest call that never returns.

import Foundation
import Network

final class HttpServer {
    private let port: UInt16
    private let router: Router
    private let session: String?
    private let requestGate = RequestGate()
    private let queue = DispatchQueue(label: "dev.tapwing.ios-runner.http", qos: .userInteractive)
    private var listener: NWListener?
    private var connections: [ObjectIdentifier: ClientConnection] = [:]

    /// `session` is echoed in `GET /status` (see `StatusResponse.session`).
    init(port: UInt16, router: Router, session: String?) {
        self.port = port
        self.router = router
        self.session = session
    }

    func start() async throws {
        guard let nwPort = NWEndpoint.Port(rawValue: port) else {
            throw NSError(
                domain: "dev.tapwing.ios-runner",
                code: 2,
                userInfo: [NSLocalizedDescriptionKey: "Invalid port \(port)"]
            )
        }

        let tcp = NWProtocolTCP.Options()
        tcp.noDelay = true
        let params = NWParameters(tls: nil, tcp: tcp)
        params.allowLocalEndpointReuse = true
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: nwPort)

        let listener = try NWListener(using: params)
        self.listener = listener

        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            let gate = StartupGate(continuation: continuation)
            listener.stateUpdateHandler = { [weak self] state in
                switch state {
                case .ready:
                    gate.succeed()
                case .failed(let error):
                    if !gate.fail(error) {
                        NSLog("[TapwingRunner] HTTP listener failed: %@", error.localizedDescription)
                        self?.listener?.cancel()
                    }
                case .cancelled:
                    _ = gate.fail(CancellationError())
                default:
                    break
                }
            }

            listener.newConnectionHandler = { [weak self] nwConn in
                guard let self else {
                    nwConn.cancel()
                    return
                }
                self.accept(nwConn)
            }

            listener.start(queue: queue)
        }
    }

    private func accept(_ nwConn: NWConnection) {
        let client = ClientConnection(
            connection: nwConn,
            queue: queue,
            router: router,
            requestGate: requestGate,
            session: session
        ) { [weak self] id in
            self?.connections.removeValue(forKey: id)
        }
        connections[client.id] = client
        client.start()
    }
}

private final class StartupGate: @unchecked Sendable {
    private var continuation: CheckedContinuation<Void, Error>?

    init(continuation: CheckedContinuation<Void, Error>) {
        self.continuation = continuation
    }

    func succeed() {
        guard let c = continuation else { return }
        continuation = nil
        c.resume(returning: ())
    }

    @discardableResult
    func fail(_ error: Error) -> Bool {
        guard let c = continuation else { return false }
        continuation = nil
        c.resume(throwing: error)
        return true
    }
}

/// One client connection. All of its state is confined to the server `queue`;
/// only route handlers run elsewhere (on the main actor) and hop back here.
private final class ClientConnection: @unchecked Sendable {
    let id: ObjectIdentifier
    private let connection: NWConnection
    private let queue: DispatchQueue
    private let router: Router
    private let requestGate: RequestGate
    private let session: String?
    private let onClose: (ObjectIdentifier) -> Void
    private var machine = HttpConnectionMachine()
    /// The request this connection waits on (at most one: no pipelining ahead of a reply).
    private var inFlight: RequestTicket?

    init(
        connection: NWConnection,
        queue: DispatchQueue,
        router: Router,
        requestGate: RequestGate,
        session: String?,
        onClose: @escaping (ObjectIdentifier) -> Void
    ) {
        self.connection = connection
        self.queue = queue
        self.router = router
        self.requestGate = requestGate
        self.session = session
        self.onClose = onClose
        self.id = ObjectIdentifier(connection)
    }

    func start() {
        connection.stateUpdateHandler = { [weak self] state in
            switch state {
            case .failed, .cancelled:
                self?.close()
            default:
                break
            }
        }
        connection.start(queue: queue)
        receiveNext()
    }

    private func receiveNext() {
        guard machine.beginReceive() else { return }
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self else { return }
            let action = self.machine.onReceive(data: data, isComplete: isComplete, hadError: error != nil)
            self.handleReadAction(action)
        }
    }

    private func processBuffer() {
        handleReadAction(machine.pollNextRequest())
    }

    private func handleReadAction(_ action: HttpConnectionMachine.ReadAction) {
        switch action {
        case .needMoreData:
            receiveNext()
        case .close:
            close()
        case .malformed(_, let wire):
            connection.send(content: wire, completion: .contentProcessed { [weak self] _ in
                self?.close()
            })
        case .statusFastPath(_, let keepAlive):
            // Answered here, off the main thread and outside the gate, so a
            // wedged handler can never make the runner look dead.
            send(HttpResponse.json(GateReply.status(requestGate.snapshot(), session: session)), keepAlive: keepAlive)
        case .dispatch(let request, let keepAlive):
            dispatch(request, keepAlive: keepAlive)
        case .ignoreWhileInFlight(let continueReading):
            if continueReading {
                receiveNext()
            }
        }
    }

    /// Queues `request` behind the gate and arms its deadline watchdog.
    private func dispatch(_ request: HttpRequest, keepAlive: Bool) {
        let label = "\(request.method) \(request.path)"
        let budget = RequestBudget.seconds(fromHeaders: request.headers)
        let router = self.router
        let requestGate = self.requestGate
        let admission = requestGate.submit(label: label, budget: budget) { [weak self] ticket in
            Task { @MainActor in
                let response = await router.handle(request, ticket: ticket)
                guard requestGate.finish(ticket), let self else { return }
                self.queue.async { self.complete(ticket, with: response, keepAlive: keepAlive) }
            }
        }
        switch admission {
        case .refused(let gate):
            machine.completeInFlight()
            send(GateReply.refused(label: label, blockedBy: gate).httpResponse, keepAlive: keepAlive)
        case .admitted(let ticket):
            inFlight = ticket
            queue.asyncAfter(deadline: .now() + budget) { [weak self] in
                let reply: AppError
                switch requestGate.expire(ticket) {
                case .alreadyAnswered:
                    return
                case .inFlight:
                    reply = GateReply.inFlight(ticket)
                case .neverStarted(let gate):
                    reply = GateReply.neverStarted(ticket, blockedBy: gate)
                }
                NSLog("[TapwingRunner] %@", reply.message)
                self?.complete(ticket, with: reply.httpResponse, keepAlive: keepAlive)
            }
            // Keep reading so a hang-up of the client is noticed while it waits.
            receiveNext()
        }
    }

    private func complete(_ ticket: RequestTicket, with response: HttpResponse, keepAlive: Bool) {
        guard !machine.isClosed, inFlight === ticket else { return }
        inFlight = nil
        machine.completeInFlight()
        send(response, keepAlive: keepAlive)
    }

    private func send(_ response: HttpResponse, keepAlive: Bool) {
        let wire = HttpFraming.serialize(response, keepAlive: keepAlive)
        connection.send(content: wire, completion: .contentProcessed { [weak self] sendError in
            guard let self, !self.machine.isClosed else { return }
            switch self.machine.onSendFinished(hadError: sendError != nil, keepAlive: keepAlive) {
            case .close:
                self.close()
            case .processNext:
                self.processBuffer()
            }
        })
    }

    private func close() {
        guard machine.markClosed() else { return }
        if let ticket = inFlight {
            inFlight = nil
            requestGate.clientGone(ticket)
        }
        connection.stateUpdateHandler = nil
        connection.cancel()
        onClose(id)
    }
}
