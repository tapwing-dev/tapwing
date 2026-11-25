// XCUITest entry point for Tapwing's iOS Simulator native-lane runner.
// Launched via `xcodebuild test-without-building` with `TEST_RUNNER_PORT=<port>`
// and `TEST_RUNNER_TAPWING_SESSION=<id>` (xcodebuild strips the prefix).

import Foundation
import XCTest

final class RunnerTest: XCTestCase {
    private static let defaultPort: UInt16 = 22087

    override func setUpWithError() throws {
        // XCTest internals can raise assertion failures on transient AX states;
        // keep the HTTP server alive instead of aborting the test runner.
        continueAfterFailure = true
    }

    @MainActor
    func testHttpServer() async throws {
        let port: UInt16
        if let raw = ProcessInfo.processInfo.environment["PORT"], let parsed = UInt16(raw), parsed > 0 {
            port = parsed
        } else {
            port = Self.defaultPort
        }

        let session = GateReply.session(fromEnvironment: ProcessInfo.processInfo.environment)
        let server = HttpServer(port: port, router: Router(), session: session)
        try await server.start()
        NSLog("[TapwingRunner] Listening on 127.0.0.1:%u", port)

        // Stay alive until Tapwing stops `xcodebuild` / terminates the runner app.
        await withCheckedContinuation { (_: CheckedContinuation<Void, Never>) in }
    }
}
