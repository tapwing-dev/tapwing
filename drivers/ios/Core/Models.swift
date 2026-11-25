// Wire models and error classification for Tapwing's iOS Simulator XCUITest
// HTTP runner. Field names and semantics follow Maestro 2.12.1
// (`maestro-ios-xctest-runner/maestro-driver-iosUITests/Routes/Models/*` and
// `Routes/Helpers/AppError.swift`, Apache-2.0, Copyright (c) 2022 mobile.dev inc.,
// modified for Tapwing).

import Foundation

// MARK: - Error envelope & XCUITest timeout classification

public enum AppErrorType: String, Codable, Equatable, Sendable {
    case `internal`
    case precondition
    case timeout
    case notFound
    /// The handler ran past the request deadline and is still running; the
    /// request may or may not have been applied (see `RequestGate`).
    case deadlineExceeded
    /// The request was never started: the runner was busy until the request
    /// deadline, or is wedged in an overdue handler.
    case runnerBusy
}

public struct AppError: Error, Codable, Equatable, Sendable {
    public let type: AppErrorType
    public let message: String

    public init(type: AppErrorType = .internal, message: String) {
        self.type = type
        self.message = message
    }

    public var statusCode: Int {
        switch type {
        case .internal: return 500
        case .precondition: return 400
        case .timeout, .deadlineExceeded, .runnerBusy: return 408
        case .notFound: return 404
        }
    }

    public var httpResponse: HttpResponse {
        HttpResponse.json(self, statusCode: statusCode)
    }

    // Encode both `errorCode` (read by Tapwing's `ios_native_lane/client.rs`)
    // and `code` (Maestro 2.12.1 Swift runner's key) for full wire compatibility.
    private enum CodingKeys: String, CodingKey {
        case errorMessage
        case errorCode
        case code
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(message, forKey: .errorMessage)
        try container.encode(type.rawValue, forKey: .errorCode)
        try container.encode(type.rawValue, forKey: .code)
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        message = try container.decode(String.self, forKey: .errorMessage)
        let raw =
            try container.decodeIfPresent(String.self, forKey: .errorCode)
            ?? container.decodeIfPresent(String.self, forKey: .code)
            ?? AppErrorType.internal.rawValue
        type = AppErrorType(rawValue: raw) ?? .internal
    }
}

public extension NSError {
    /// True when this error is one of the two deterministic XCUITest
    /// "app never reached idle" timeouts (`XCTFuture` / 1000 or
    /// `automation-support.error` / 6), which must be surfaced as HTTP 408.
    var isXCUITestTimeout: Bool {
        (domain == "com.apple.dt.XCTest.XCTFuture"
            && code == 1000
            && localizedDescription.contains("Timed out while evaluating UI query"))
        || (domain == "com.apple.dt.xctest.automation-support.error"
            && code == 6
            && localizedDescription.contains("Unable to perform work on main run loop, process main thread busy for"))
    }
}

/// Maps a swipe failure to an `AppError`: an XCUITest timeout becomes `.timeout`
/// (HTTP 408), while any other error stays `.internal` (HTTP 500).
public func classifySwipeError(_ error: Error) -> AppError {
    let message = "Swipe v2 request failure. Error: \(error.localizedDescription)"
    if (error as NSError).isXCUITestTimeout {
        return AppError(type: .timeout, message: message)
    }
    return AppError(type: .internal, message: message)
}

// MARK: - Hierarchy models

public typealias AXFrame = [String: Double]

public extension AXFrame {
    static var zero: AXFrame {
        ["X": 0, "Y": 0, "Width": 0, "Height": 0]
    }

    var x: Double { self["X"] ?? 0 }
    var y: Double { self["Y"] ?? 0 }
    var width: Double { self["Width"] ?? 0 }
    var height: Double { self["Height"] ?? 0 }
}

public struct AXElement: Codable, Equatable, Sendable {
    public let identifier: String
    public let frame: AXFrame
    public let value: String?
    public let title: String?
    public let label: String
    public let elementType: Int
    public let enabled: Bool
    public let horizontalSizeClass: Int
    public let verticalSizeClass: Int
    public let placeholderValue: String?
    public let selected: Bool
    public let hasFocus: Bool
    public let displayID: Int
    public let windowContextID: Double
    public var children: [AXElement]?

    public init(
        identifier: String = "",
        frame: AXFrame = .zero,
        value: String? = nil,
        title: String? = nil,
        label: String = "",
        elementType: Int = 0,
        enabled: Bool = false,
        horizontalSizeClass: Int = 0,
        verticalSizeClass: Int = 0,
        placeholderValue: String? = nil,
        selected: Bool = false,
        hasFocus: Bool = false,
        displayID: Int = 0,
        windowContextID: Double = 0,
        children: [AXElement]? = nil
    ) {
        self.identifier = identifier
        self.frame = frame
        self.value = value
        self.title = title
        self.label = label
        self.elementType = elementType
        self.enabled = enabled
        self.horizontalSizeClass = horizontalSizeClass
        self.verticalSizeClass = verticalSizeClass
        self.placeholderValue = placeholderValue
        self.selected = selected
        self.hasFocus = hasFocus
        self.displayID = displayID
        self.windowContextID = windowContextID
        self.children = children
    }

    public func depth() -> Int {
        guard let children, !children.isEmpty else {
            return 1
        }
        return (children.map { $0.depth() }.max() ?? 0) + 1
    }
}

public struct ViewHierarchyResponse: Codable, Equatable, Sendable {
    public let axElement: AXElement
    public let depth: Int

    public init(axElement: AXElement, depth: Int) {
        self.axElement = axElement
        self.depth = depth
    }
}

// MARK: - Request / Response DTOs for all 14 routes

/// `GET /status`. Only `status` is always present; the optional fields are
/// omitted when nil, so an idle runner still answers `{"status":"ok"}`.
public struct StatusResponse: Codable, Equatable, Sendable {
    /// `"ok"`, or `"wedged"` while a handler is past its deadline and still running.
    public let status: String
    /// Label of the handler holding the request gate (`"POST /viewHierarchy"`).
    public let busy: String?
    /// How long that handler has been running, in milliseconds.
    public let busyMs: Int?
    public let wedged: Bool?
    /// The session id the host started this runner with (`TEST_RUNNER_TAPWING_SESSION`),
    /// echoed so the host can tell this runner from a stale one on the same port.
    public let session: String?

    public init(
        status: String = "ok",
        busy: String? = nil,
        busyMs: Int? = nil,
        wedged: Bool? = nil,
        session: String? = nil
    ) {
        self.status = status
        self.busy = busy
        self.busyMs = busyMs
        self.wedged = wedged
        self.session = session
    }
}

public struct DeviceInfoResponse: Codable, Equatable, Sendable {
    public let widthPoints: Int
    public let heightPoints: Int
    public let widthPixels: Int
    public let heightPixels: Int

    public init(widthPoints: Int, heightPoints: Int, widthPixels: Int, heightPixels: Int) {
        self.widthPoints = widthPoints
        self.heightPoints = heightPoints
        self.widthPixels = widthPixels
        self.heightPixels = heightPixels
    }
}

public struct ViewHierarchyRequest: Codable, Equatable, Sendable {
    public let appIds: [String]?
    public let excludeKeyboardElements: Bool?

    public init(appIds: [String]? = nil, excludeKeyboardElements: Bool? = nil) {
        self.appIds = appIds
        self.excludeKeyboardElements = excludeKeyboardElements
    }
}

public struct TouchRequest: Codable, Equatable, Sendable {
    public let x: Double
    public let y: Double
    public let duration: TimeInterval?

    public init(x: Double, y: Double, duration: TimeInterval? = nil) {
        self.x = x
        self.y = y
        self.duration = duration
    }
}

public struct SwipeV2Request: Codable, Equatable, Sendable {
    public let appId: String?
    public let startX: Double
    public let startY: Double
    public let endX: Double
    public let endY: Double
    public let duration: TimeInterval
    public let appIds: [String]?

    public init(
        appId: String? = nil,
        startX: Double,
        startY: Double,
        endX: Double,
        endY: Double,
        duration: TimeInterval,
        appIds: [String]? = nil
    ) {
        self.appId = appId
        self.startX = startX
        self.startY = startY
        self.endX = endX
        self.endY = endY
        self.duration = duration
        self.appIds = appIds
    }
}

public struct InputTextRequest: Codable, Equatable, Sendable {
    public let text: String
    public let appIds: [String]?

    public init(text: String, appIds: [String]? = nil) {
        self.text = text
        self.appIds = appIds
    }
}

public struct EraseTextRequest: Codable, Equatable, Sendable {
    public let charactersToErase: Int
    public let appIds: [String]?

    public init(charactersToErase: Int, appIds: [String]? = nil) {
        self.charactersToErase = charactersToErase
        self.appIds = appIds
    }
}

public struct PressKeyRequest: Codable, Equatable, Sendable {
    public enum Key: String, Codable, Equatable, Sendable {
        case delete
        case `return`
        case enter
        case tab
        case space
        case escape
    }

    public let key: Key

    public init(key: Key) {
        self.key = key
    }
}

public struct PressButtonRequest: Codable, Equatable, Sendable {
    public enum Button: String, Codable, Equatable, Sendable {
        case home
        case lock
    }

    public let button: Button

    public init(button: Button) {
        self.button = button
    }
}

public struct SetOrientationRequest: Codable, Equatable, Sendable {
    public enum Orientation: String, Codable, Equatable, Sendable {
        case portrait
        case landscapeLeft
        case landscapeRight
        case upsideDown
    }

    public let orientation: Orientation

    public init(orientation: Orientation) {
        self.orientation = orientation
    }
}

public struct RunningAppRequest: Codable, Equatable, Sendable {
    public let appIds: [String]

    public init(appIds: [String]) {
        self.appIds = appIds
    }
}

public struct RunningAppResponse: Codable, Equatable, Sendable {
    public let runningAppBundleId: String

    public init(runningAppBundleId: String) {
        self.runningAppBundleId = runningAppBundleId
    }
}

public struct KeyboardRequest: Codable, Equatable, Sendable {
    public let appIds: [String]

    public init(appIds: [String]) {
        self.appIds = appIds
    }
}

public struct KeyboardResponse: Codable, Equatable, Sendable {
    public let isKeyboardVisible: Bool

    public init(isKeyboardVisible: Bool) {
        self.isKeyboardVisible = isKeyboardVisible
    }
}

public struct ScreenStaticResponse: Codable, Equatable, Sendable {
    public let isScreenStatic: Bool

    public init(isScreenStatic: Bool) {
        self.isScreenStatic = isScreenStatic
    }
}
