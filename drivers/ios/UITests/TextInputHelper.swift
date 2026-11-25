// Keyboard readiness wait and paced text synthesis (ASCII + Unicode).
// Ported from Maestro 2.12.1 (`Routes/Helpers/TextInputHelper.swift`,
// Apache-2.0, Copyright (c) 2022 mobile.dev inc., modified for Tapwing).

import Foundation
import XCTest

/// Thread-safe one-shot wrapper around an ObjC-bridged `CheckedContinuation` so
/// a duplicated completion callback can never trap the Swift runtime.
final class ContinuationOnceGuard: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Error>?

    init(_ continuation: CheckedContinuation<Void, Error>) {
        self.continuation = continuation
    }

    func resume(error: Error?) {
        lock.lock()
        let pending = continuation
        continuation = nil
        lock.unlock()
        guard let pending else { return }
        if let error {
            pending.resume(throwing: error)
        } else {
            pending.resume(returning: ())
        }
    }
}

@MainActor
enum TextInputHelper {
    private static let typingFrequency: UInt64 = 30

    /// Waits up to 1 second for the soft keyboard to appear when a text field
    /// was just focused.
    static func waitUntilKeyboardIsPresented() async {
        let deadline = Date().addingTimeInterval(1.0)
        while Date() < deadline {
            let app = RunningApp.getForegroundApp()
                ?? XCUIApplication(bundleIdentifier: RunningApp.springboardBundleId)
            if app.keyboards.firstMatch.exists {
                return
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
    }

    /// Synthesizes text via `XCPointerEventPath.typeText:atOffset:typingSpeed:shouldRedact:`.
    /// Sends the first character slowly followed by a 500 ms settle pause so iOS
    /// autocorrection / hardware-keyboard listeners do not drop characters after
    /// the first one (preserving exact Maestro 2.12.1 reliability, including
    /// non-ASCII Unicode such as Czech diacritics).
    ///
    /// The rest is typed in `TypingPlan` chunks; between chunks `shouldStop`
    /// (the request's deadline / client-gone flag) is checked and typing stops
    /// with `TypingStopped` once the request is dead. A chunk already handed
    /// to XCTest cannot be cancelled.
    static func inputText(_ text: String, shouldStop: () -> Bool) async throws {
        var plan = TypingPlan(text: text)
        guard plan.hasMore else { return }

        let orientation = ScreenSizeHelper.currentInterfaceOrientation()
        while let step = try plan.next(shouldStop: shouldStop()) {
            try await synthesizeText(step.text, typingSpeed: step.slow ? 1 : typingFrequency, orientation: orientation)
            if step.slow && plan.hasMore {
                try await Task.sleep(nanoseconds: 500_000_000)
            }
        }
    }

    static func synthesizeText(
        _ text: String,
        typingSpeed: UInt64,
        orientation: UIInterfaceOrientation
    ) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            let once = ContinuationOnceGuard(continuation)
            TapwingSynthesizeTextInput(text, typingSpeed, orientation) { error in
                once.resume(error: error)
            }
        }
    }
}
