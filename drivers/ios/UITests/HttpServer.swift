// Zero-dependency HTTP/1.1 keep-alive loopback server using `Network.framework`.
// Binds to `127.0.0.1:<port>` with `TCP_NODELAY` enabled so persistent
// `ureq::Agent` connections from Tapwing incur zero Nagle/delayed-ACK latency.

import Foundation
import Network

final class HttpServer {
    private let port: UInt16
    private let router: Router
    private let requestGate = RequestGate()
    private let queue = DispatchQueue(label: "dev.tapwing.ios-runner.http", qos: .userInteractive)
    private var listener: NWListener?
    private var connections: [ObjectIdentifier: ClientConnection] = [:]

    init(port: UInt16, router: Router) {
        self.port = port
        self.router = router
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
            requestGate: requestGate
        ) { [weak self] id in
            self?.connections.removeValue(forKey: id)
        }
        connections[client.id] = client
        client.start()
    }
}

/// Async FIFO gate serializing `router.handle(_:)` across concurrent client
/// connections so host timeout + reconnect can never interleave `@MainActor`
/// XCUITest operations across `await` suspension points.
private actor RequestGate {
    private var isBusy = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func acquire() async {
        if !isBusy {
            isBusy = true
            return
        }
        await withCheckedContinuation { continuation in
            waiters.append(continuation)
        }
    }

    func release() {
        if waiters.isEmpty {
            isBusy = false
        } else {
            let next = waiters.removeFirst()
            next.resume()
        }
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

private final class ClientConnection: @unchecked Sendable {
    let id: ObjectIdentifier
    private let connection: NWConnection
    private let queue: DispatchQueue
    private let router: Router
    private let requestGate: RequestGate
    private let onClose: (ObjectIdentifier) -> Void
    private var buffer = Data()
    private var closed = false

    init(
        connection: NWConnection,
        queue: DispatchQueue,
        router: Router,
        requestGate: RequestGate,
        onClose: @escaping (ObjectIdentifier) -> Void
    ) {
        self.connection = connection
        self.queue = queue
        self.router = router
        self.requestGate = requestGate
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
        guard !closed else { return }
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, isComplete, error in
            guard let self, !self.closed else { return }
            if let data, !data.isEmpty {
                self.buffer.append(data)
            }
            if error != nil {
                self.close()
                return
            }
            self.processBuffer(isEOF: isComplete)
        }
    }

    private func processBuffer(isEOF: Bool) {
        guard !closed else { return }
        switch HttpFraming.tryParseRequest(from: &buffer) {
        case .incomplete:
            if isEOF {
                close()
            } else {
                receiveNext()
            }
        case .malformed(let reason):
            let resp = AppError(type: .precondition, message: reason).httpResponse
            let wire = HttpFraming.serialize(resp, keepAlive: false)
            connection.send(content: wire, completion: .contentProcessed { [weak self] _ in
                self?.close()
            })
        case .complete(let request):
            let router = self.router
            let requestGate = self.requestGate
            let keepAlive = request.shouldKeepAlive && !isEOF
            Task { @MainActor [weak self] in
                await requestGate.acquire()
                let response = await router.handle(request)
                await requestGate.release()
                let wire = HttpFraming.serialize(response, keepAlive: keepAlive)
                guard let self else { return }
                self.queue.async { [weak self] in
                    guard let self, !self.closed else { return }
                    self.connection.send(content: wire, completion: .contentProcessed { [weak self] sendError in
                        guard let self, !self.closed else { return }
                        if sendError != nil || !keepAlive {
                            self.close()
                        } else {
                            self.processBuffer(isEOF: isEOF)
                        }
                    })
                }
            }
        }
    }

    private func close() {
        guard !closed else { return }
        closed = true
        connection.stateUpdateHandler = nil
        connection.cancel()
        onClose(id)
    }
}
