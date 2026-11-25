// Pure caching and query-gating policies for Tapwing's iOS Simulator XCUITest
// runner, shared with macOS host unit tests (`swift test`).
//
// Everything here is value-typed and clock-injected so cache hit/miss, active-PID
// pruning, TTL expiry, orientation invalidation, and SafariViewService gating are
// deterministically tested without a booted simulator or sleeps.

import Foundation

// MARK: - PID -> Bundle ID cache model

/// Pure cache policy matching `TapwingActiveAppsInfo()` in `XCTestRuntimeBridge.m`.
///
/// Active PIDs are always supplied live by the caller (`[axClient activeApplications]`).
/// Every call prunes cached entries whose PID is no longer active so terminated
/// processes are evicted immediately (preventing stale bundle IDs on PID reuse and
/// bounding cache size to the active process count). Transient lookup failures
/// (`nil`, empty string, or `"unknownBundleId"`) are never cached.
public struct PidBundleIdCache: Sendable {
    public static let unknownBundleId = "unknownBundleId"

    private var entries: [Int32: String] = [:]

    public init() {}

    public var count: Int {
        entries.count
    }

    public func cachedBundleId(for pid: Int32) -> String? {
        entries[pid]
    }

    /// Resolves bundle IDs for `activePids`, pruning any cached PID not present in
    /// `activePids` and invoking `lookup` only for uncached PIDs.
    public mutating func resolve(
        activePids: [Int32],
        lookup: (Int32) -> String?
    ) -> [(pid: Int32, bundleId: String)] {
        let activeSet = Set(activePids)
        entries = entries.filter { activeSet.contains($0.key) }

        var resolvedInCall: [Int32: String] = [:]
        resolvedInCall.reserveCapacity(activeSet.count)

        var results: [(pid: Int32, bundleId: String)] = []
        results.reserveCapacity(activePids.count)

        for pid in activePids {
            if let cached = entries[pid] ?? resolvedInCall[pid] {
                results.append((pid: pid, bundleId: cached))
                continue
            }
            if let resolved = lookup(pid), !resolved.isEmpty, resolved != Self.unknownBundleId {
                resolvedInCall[pid] = resolved
                if pid > 0 {
                    entries[pid] = resolved
                }
                results.append((pid: pid, bundleId: resolved))
            } else {
                results.append((pid: pid, bundleId: Self.unknownBundleId))
            }
        }
        return results
    }

    public mutating func invalidateAll() {
        entries.removeAll()
    }
}

// MARK: - View hierarchy query gating

/// Pure gating rules that avoid redundant cross-process AX/XPC queries on
/// `POST /viewHierarchy`.
public enum HierarchyQueryPolicy {
    public static let springboardBundleId = "com.apple.springboard"
    public static let dockFolderViewServiceBundleId = "com.apple.DocumentManager.DockFolderViewService"
    public static let safariViewServiceBundleId = "com.apple.SafariViewService"

    /// True when the foreground snapshot is taken from SpringBoard itself, so
    /// status bars and the device frame are already part of `appHierarchy`.
    public static func isSpringboardTarget(foregroundBundleId: String) -> Bool {
        foregroundBundleId == springboardBundleId
            || foregroundBundleId == dockFolderViewServiceBundleId
    }

    /// True when `HierarchyBuilder` should probe `com.apple.SafariViewService`
    /// to merge an embedded `SFSafariViewController` / `ASWebAuthenticationSession`
    /// web tree on iOS 26+.
    ///
    /// Embedded Safari view services are hosted inside the foreground app via a
    /// remote accessibility element (`isRemote == true`). When the foreground tree
    /// contains zero remote elements (or is already `SafariViewService`), skipping
    /// the probe avoids cross-process XPCs with zero risk of missing a web sheet.
    public static func shouldQuerySafariViewService(
        osMajorVersion: Int,
        foregroundBundleId: String,
        hasRemoteElement: Bool
    ) -> Bool {
        guard osMajorVersion >= 26 else { return false }
        guard foregroundBundleId != safariViewServiceBundleId else { return false }
        return hasRemoteElement
    }
}

// MARK: - View hierarchy SpringBoard metadata cache

/// Caches slowly-changing SpringBoard metadata used by `HierarchyBuilder`:
///
/// 1. `deviceAxFrame` (`springboardApplication.frame`) keyed by `LogicalOrientation`.
///    Invalid (non-positive or non-finite) frames are never cached.
/// 2. `statusBars` (`fullStatusBars(springboardApplication)`) keyed by
///    `(LogicalOrientation, foregroundBundleId)` with a short monotonic TTL
///    (default `3.0 s`) so 50 ms `poll_find` loops do not issue repeated
///    SpringBoard AX IPCs. Failed fetches (`nil`) are never cached.
///
/// Both caches are cleared immediately on `invalidateAll()` (called by
/// `Actions.setOrientation`).
public struct HierarchyMetadataCache: Sendable {
    public static let defaultStatusBarTTL: TimeInterval = 3.0

    public struct StatusBarKey: Equatable, Hashable, Sendable {
        public let orientation: LogicalOrientation
        public let foregroundBundleId: String

        public init(orientation: LogicalOrientation, foregroundBundleId: String) {
            self.orientation = orientation
            self.foregroundBundleId = foregroundBundleId
        }
    }

    private struct CachedStatusBars: Sendable {
        let key: StatusBarKey
        let elements: [AXElement]
        let cachedAt: TimeInterval
    }

    public let statusBarTTL: TimeInterval
    private let now: @Sendable () -> TimeInterval
    private var deviceFrames: [LogicalOrientation: AXFrame] = [:]
    private var cachedStatusBars: CachedStatusBars?

    public init(
        statusBarTTL: TimeInterval = defaultStatusBarTTL,
        now: @escaping @Sendable () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }
    ) {
        self.statusBarTTL = statusBarTTL
        self.now = now
    }

    public var cachedDeviceFrameCount: Int {
        deviceFrames.count
    }

    public var hasCachedStatusBars: Bool {
        cachedStatusBars != nil
    }

    /// Returns the cached SpringBoard `deviceAxFrame` for `orientation`, or invokes
    /// `fetch` once and caches the result when dimensions are positive and finite.
    public mutating func deviceFrame(
        for orientation: LogicalOrientation,
        fetch: () -> AXFrame
    ) -> AXFrame {
        if let cached = deviceFrames[orientation] {
            return cached
        }
        let fetched = fetch()
        if Self.isValidDeviceFrame(fetched) {
            deviceFrames[orientation] = fetched
        }
        return fetched
    }

    /// Returns cached status bars when `(orientation, foregroundBundleId)` matches
    /// and the entry is younger than `statusBarTTL`; otherwise calls `fetch`.
    /// A `nil` return from `fetch` indicates a transient snapshot error and is
    /// not cached (the caller receives `[]`).
    public mutating func statusBars(
        orientation: LogicalOrientation,
        foregroundBundleId: String,
        fetch: () -> [AXElement]?
    ) -> [AXElement] {
        let key = StatusBarKey(orientation: orientation, foregroundBundleId: foregroundBundleId)
        let currentTime = now()
        if let cached = cachedStatusBars, cached.key == key {
            let age = currentTime - cached.cachedAt
            if age >= 0 && age < statusBarTTL {
                return cached.elements
            }
        }
        guard let fetched = fetch() else {
            cachedStatusBars = nil
            return []
        }
        cachedStatusBars = CachedStatusBars(
            key: key,
            elements: fetched,
            cachedAt: now()
        )
        return fetched
    }

    /// Clears all cached SpringBoard frames and status bars (e.g. on orientation change).
    public mutating func invalidateAll() {
        deviceFrames.removeAll()
        cachedStatusBars = nil
    }

    public static func isValidDeviceFrame(_ frame: AXFrame) -> Bool {
        guard let x = frame["X"], let y = frame["Y"],
              let width = frame["Width"], let height = frame["Height"],
              x.isFinite, y.isFinite, width.isFinite, height.isFinite,
              width > 0, height > 0 else {
            return false
        }
        return true
    }
}
