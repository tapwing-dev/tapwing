// Splits text for `inputText` / `eraseText` into paced steps so the typing loop
// can stop between them once its request is dead (deadline passed or client
// gone) instead of typing into the app for minutes after the host gave up.

import Foundation

public struct TypingStep: Equatable, Sendable {
    public let text: String
    /// The slow first character (see `TextInputHelper.inputText`).
    public let slow: Bool

    public init(text: String, slow: Bool) {
        self.text = text
        self.slow = slow
    }
}

/// Typing stopped early because nobody waits for the request any more.
public struct TypingStopped: Error, Equatable, Sendable {
    public let typedCharacters: Int
    public let totalCharacters: Int

    public init(typedCharacters: Int, totalCharacters: Int) {
        self.typedCharacters = typedCharacters
        self.totalCharacters = totalCharacters
    }

    public var appError: AppError {
        AppError(
            type: .deadlineExceeded,
            message: "Stopped typing after \(typedCharacters) of \(totalCharacters) characters: "
                + "the request deadline passed or the client disconnected"
        )
    }
}

public struct TypingPlan: Sendable {
    /// Characters typed slowly first so iOS does not drop what follows.
    public static let slowPrefixCount = 1
    /// Characters per synthesized event after the prefix: about 2 s at the
    /// runner's 30 characters per second, which bounds how long typing can
    /// overrun a dead request. Text up to `slowPrefixCount + chunkSize`
    /// characters is typed exactly as before chunking existed (one event).
    public static let chunkSize = 60

    public let steps: [TypingStep]
    public let totalCharacters: Int
    public private(set) var typedCharacters = 0
    private var nextIndex = 0

    /// Steps never split a `Character` (grapheme cluster), so emoji sequences
    /// and combining marks reach XCTest whole.
    public init(text: String, chunkSize: Int = TypingPlan.chunkSize) {
        let size = max(1, chunkSize)
        var steps: [TypingStep] = []
        let prefix = String(text.prefix(Self.slowPrefixCount))
        if !prefix.isEmpty {
            steps.append(TypingStep(text: prefix, slow: true))
        }
        var rest = text.dropFirst(Self.slowPrefixCount)
        while !rest.isEmpty {
            steps.append(TypingStep(text: String(rest.prefix(size)), slow: false))
            rest = rest.dropFirst(size)
        }
        self.steps = steps
        self.totalCharacters = text.count
    }

    public var hasMore: Bool {
        nextIndex < steps.count
    }

    /// The next step to type, or nil when everything was typed. Throws
    /// `TypingStopped` when text is left but `shouldStop` is true.
    public mutating func next(shouldStop: Bool) throws -> TypingStep? {
        guard hasMore else { return nil }
        if shouldStop {
            throw TypingStopped(typedCharacters: typedCharacters, totalCharacters: totalCharacters)
        }
        let step = steps[nextIndex]
        nextIndex += 1
        typedCharacters += step.text.count
        return step
    }
}
