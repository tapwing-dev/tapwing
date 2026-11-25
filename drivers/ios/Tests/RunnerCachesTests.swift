import Foundation
import XCTest
@testable import TapwingRunnerCore

/// Hand-driven monotonic clock so TTL tests never sleep or depend on wall time.
private final class ManualClock: @unchecked Sendable {
    private let lock = NSLock()
    private var value: TimeInterval

    init(_ initial: TimeInterval = 100.0) {
        self.value = initial
    }

    var now: TimeInterval {
        lock.lock()
        defer { lock.unlock() }
        return value
    }

    func set(_ newValue: TimeInterval) {
        lock.lock()
        value = newValue
        lock.unlock()
    }
}

final class RunnerCachesTests: XCTestCase {

    // MARK: - PID -> Bundle ID cache

    func testPidCacheHitsOnRepeatedWarmCallsAndSkipsLookup() {
        var cache = PidBundleIdCache()
        var lookupCalls: [Int32] = []
        let bundleByPid: [Int32: String] = [
            101: "com.apple.springboard",
            202: "dev.tapwing.sampleApp",
            303: "com.apple.Preferences",
        ]

        let first = cache.resolve(activePids: [101, 202, 303]) { pid in
            lookupCalls.append(pid)
            return bundleByPid[pid]
        }
        XCTAssertEqual(first.map(\.bundleId), [
            "com.apple.springboard",
            "dev.tapwing.sampleApp",
            "com.apple.Preferences",
        ])
        XCTAssertEqual(lookupCalls, [101, 202, 303])
        XCTAssertEqual(cache.count, 3)

        // Warm polls issue 0 lookups.
        lookupCalls.removeAll()
        for _ in 0..<20 {
            let warm = cache.resolve(activePids: [101, 202, 303]) { pid in
                lookupCalls.append(pid)
                return bundleByPid[pid]
            }
            XCTAssertEqual(warm.map(\.bundleId), [
                "com.apple.springboard",
                "dev.tapwing.sampleApp",
                "com.apple.Preferences",
            ])
        }
        XCTAssertTrue(lookupCalls.isEmpty)
        XCTAssertEqual(cache.count, 3)
    }

    func testPidCachePrunesTerminatedPidsAndIsSafeOnPidReuse() {
        var cache = PidBundleIdCache()
        var lookups = 0

        _ = cache.resolve(activePids: [101, 202]) { pid in
            lookups += 1
            return pid == 101 ? "com.apple.springboard" : "com.example.oldApp"
        }
        XCTAssertEqual(lookups, 2)
        XCTAssertEqual(cache.count, 2)

        // PID 202 terminates: pruned immediately on next call.
        let afterExit = cache.resolve(activePids: [101]) { _ in
            lookups += 1
            return "unexpected"
        }
        XCTAssertEqual(afterExit.map(\.bundleId), ["com.apple.springboard"])
        XCTAssertEqual(lookups, 2)
        XCTAssertEqual(cache.count, 1)
        XCTAssertNil(cache.cachedBundleId(for: 202))

        // OS later reuses PID 202 for a new app: must miss and resolve new bundle ID.
        let afterReuse = cache.resolve(activePids: [101, 202]) { pid in
            lookups += 1
            XCTAssertEqual(pid, 202)
            return "com.example.newApp"
        }
        XCTAssertEqual(afterReuse.map(\.bundleId), ["com.apple.springboard", "com.example.newApp"])
        XCTAssertEqual(lookups, 3)
        XCTAssertEqual(cache.cachedBundleId(for: 202), "com.example.newApp")

        // Empty active PID list clears all cached entries.
        let empty = cache.resolve(activePids: []) { _ in
            XCTFail("Lookup should not be called for empty activePids")
            return nil
        }
        XCTAssertTrue(empty.isEmpty)
        XCTAssertEqual(cache.count, 0)
    }

    func testPidCacheNeverCachesNilEmptyUnknownOrNonPositivePids() {
        var cache = PidBundleIdCache()
        var attempts = 0

        let first = cache.resolve(activePids: [404, 405, 406, 0, -1]) { pid in
            attempts += 1
            switch pid {
            case 404: return nil
            case 405: return ""
            case 406: return PidBundleIdCache.unknownBundleId
            default: return "com.example.invalidPid"
            }
        }
        XCTAssertEqual(attempts, 5)
        XCTAssertEqual(first[0].bundleId, PidBundleIdCache.unknownBundleId)
        XCTAssertEqual(first[1].bundleId, PidBundleIdCache.unknownBundleId)
        XCTAssertEqual(first[2].bundleId, PidBundleIdCache.unknownBundleId)
        XCTAssertEqual(cache.count, 0)

        // Transiently starting app (404) resolves on the next poll and is then cached.
        attempts = 0
        let second = cache.resolve(activePids: [404]) { _ in
            attempts += 1
            return "com.example.nowReady"
        }
        XCTAssertEqual(attempts, 1)
        XCTAssertEqual(second.first?.bundleId, "com.example.nowReady")
        XCTAssertEqual(cache.count, 1)

        // Deduplicates duplicate PIDs within the same call.
        attempts = 0
        let dupes = cache.resolve(activePids: [404, 505, 505]) { pid in
            attempts += 1
            return pid == 505 ? "com.example.dupe" : nil
        }
        XCTAssertEqual(attempts, 1)
        XCTAssertEqual(dupes.map(\.bundleId), ["com.example.nowReady", "com.example.dupe", "com.example.dupe"])
    }

    // MARK: - Device frame cache

    func testDeviceFrameCachesPerOrientationAndRejectsInvalidFrames() {
        var cache = HierarchyMetadataCache()
        var fetches = 0
        let portrait: AXFrame = ["X": 0, "Y": 0, "Width": 393, "Height": 852]
        let landscape: AXFrame = ["X": 0, "Y": 0, "Width": 852, "Height": 393]

        // Zero / negative / NaN frames are returned but never cached.
        let zeroFrame = cache.deviceFrame(for: .portrait) {
            fetches += 1
            return ["X": 0, "Y": 0, "Width": 0, "Height": 852]
        }
        XCTAssertEqual(zeroFrame["Width"], 0)
        XCTAssertEqual(cache.cachedDeviceFrameCount, 0)

        let nanFrame = cache.deviceFrame(for: .portrait) {
            fetches += 1
            return ["X": 0, "Y": 0, "Width": .nan, "Height": 852]
        }
        XCTAssertTrue(nanFrame["Width"]?.isNaN ?? false)
        XCTAssertEqual(cache.cachedDeviceFrameCount, 0)

        // Valid portrait frame is cached.
        let p1 = cache.deviceFrame(for: .portrait) {
            fetches += 1
            return portrait
        }
        let p2 = cache.deviceFrame(for: .portrait) {
            fetches += 1
            return landscape
        }
        XCTAssertEqual(p1, portrait)
        XCTAssertEqual(p2, portrait)
        XCTAssertEqual(fetches, 3)
        XCTAssertEqual(cache.cachedDeviceFrameCount, 1)

        // Different orientation fetches once and caches independently.
        let l1 = cache.deviceFrame(for: .landscapeLeft) {
            fetches += 1
            return landscape
        }
        let l2 = cache.deviceFrame(for: .landscapeLeft) {
            fetches += 1
            return portrait
        }
        XCTAssertEqual(l1, landscape)
        XCTAssertEqual(l2, landscape)
        XCTAssertEqual(fetches, 4)
        XCTAssertEqual(cache.cachedDeviceFrameCount, 2)

        // Invalidation (e.g. POST /setOrientation) forces fresh fetch.
        cache.invalidateAll()
        XCTAssertEqual(cache.cachedDeviceFrameCount, 0)
        let p3 = cache.deviceFrame(for: .portrait) {
            fetches += 1
            return portrait
        }
        XCTAssertEqual(p3, portrait)
        XCTAssertEqual(fetches, 5)
    }

    // MARK: - Status bar cache (TTL, key switching, invalidation, error handling)

    func testStatusBarsCacheHitsWithinTTLAndRefreshesOnExpiry() {
        let clock = ManualClock(100.0)
        var cache = HierarchyMetadataCache(statusBarTTL: 3.0, now: { clock.now })
        var fetches = 0

        let barV1 = [AXElement(identifier: "sb", label: "9:41", elementType: 42, enabled: true)]
        let barV2 = [AXElement(identifier: "sb", label: "9:42", elementType: 42, enabled: true)]

        let r1 = cache.statusBars(orientation: .portrait, foregroundBundleId: "dev.tapwing.app") {
            fetches += 1
            return barV1
        }
        XCTAssertEqual(r1, barV1)
        XCTAssertEqual(fetches, 1)

        // Repeated 50 ms polls inside 3.0 s TTL hit cache with 0 fetches.
        for step in 1...59 {
            clock.set(100.0 + Double(step) * 0.05) // up to 102.95 s
            let r = cache.statusBars(orientation: .portrait, foregroundBundleId: "dev.tapwing.app") {
                fetches += 1
                return barV2
            }
            XCTAssertEqual(r, barV1)
        }
        XCTAssertEqual(fetches, 1)

        // At exactly TTL boundary (3.0 s), cache expires and refreshes.
        clock.set(103.0)
        let r2 = cache.statusBars(orientation: .portrait, foregroundBundleId: "dev.tapwing.app") {
            fetches += 1
            return barV2
        }
        XCTAssertEqual(r2, barV2)
        XCTAssertEqual(fetches, 2)
    }

    func testStatusBarsInvalidateOnOrientationAppSwitchAndInvalidateAll() {
        let clock = ManualClock(50.0)
        var cache = HierarchyMetadataCache(statusBarTTL: 3.0, now: { clock.now })
        var fetches = 0
        let bar = [AXElement(label: "status", elementType: 42, enabled: true)]

        _ = cache.statusBars(orientation: .portrait, foregroundBundleId: "com.app.a") {
            fetches += 1
            return bar
        }
        XCTAssertEqual(fetches, 1)

        // Foreground app change triggers miss.
        clock.set(50.1)
        _ = cache.statusBars(orientation: .portrait, foregroundBundleId: "com.app.b") {
            fetches += 1
            return bar
        }
        XCTAssertEqual(fetches, 2)

        // Returning to app A also fetches fresh (single-slot cache avoids stale breadcrumb pills).
        clock.set(50.2)
        _ = cache.statusBars(orientation: .portrait, foregroundBundleId: "com.app.a") {
            fetches += 1
            return bar
        }
        XCTAssertEqual(fetches, 3)

        // Orientation change triggers miss.
        clock.set(50.3)
        _ = cache.statusBars(orientation: .landscapeLeft, foregroundBundleId: "com.app.a") {
            fetches += 1
            return bar
        }
        XCTAssertEqual(fetches, 4)

        // Explicit invalidateAll (POST /setOrientation) clears cache immediately.
        cache.invalidateAll()
        XCTAssertFalse(cache.hasCachedStatusBars)
        clock.set(50.4)
        _ = cache.statusBars(orientation: .landscapeLeft, foregroundBundleId: "com.app.a") {
            fetches += 1
            return bar
        }
        XCTAssertEqual(fetches, 5)
    }

    func testStatusBarsDoesNotCacheTransientNilFailureButCachesLegitimateEmptyArray() {
        let clock = ManualClock(10.0)
        var cache = HierarchyMetadataCache(statusBarTTL: 3.0, now: { clock.now })
        var fetches = 0

        // Transient error (nil) returns [] without caching.
        let failed = cache.statusBars(orientation: .portrait, foregroundBundleId: "com.app.a") {
            fetches += 1
            return nil
        }
        XCTAssertTrue(failed.isEmpty)
        XCTAssertFalse(cache.hasCachedStatusBars)
        XCTAssertEqual(fetches, 1)

        // Next call immediately retries and caches legitimate empty array (full-screen app).
        clock.set(10.05)
        let empty = cache.statusBars(orientation: .portrait, foregroundBundleId: "com.app.a") {
            fetches += 1
            return []
        }
        XCTAssertTrue(empty.isEmpty)
        XCTAssertTrue(cache.hasCachedStatusBars)
        XCTAssertEqual(fetches, 2)

        // Subsequent poll hits the cached [].
        clock.set(10.10)
        let emptyHit = cache.statusBars(orientation: .portrait, foregroundBundleId: "com.app.a") {
            fetches += 1
            return [AXElement(label: "should not be fetched")]
        }
        XCTAssertTrue(emptyHit.isEmpty)
        XCTAssertEqual(fetches, 2)
    }

    // MARK: - Query gating (SpringBoard & SafariViewService remote-subtree gating)

    func testSpringboardTargetDetection() {
        XCTAssertTrue(HierarchyQueryPolicy.isSpringboardTarget(foregroundBundleId: "com.apple.springboard"))
        XCTAssertTrue(HierarchyQueryPolicy.isSpringboardTarget(foregroundBundleId: "com.apple.DocumentManager.DockFolderViewService"))
        XCTAssertFalse(HierarchyQueryPolicy.isSpringboardTarget(foregroundBundleId: "dev.tapwing.sampleApp"))
        XCTAssertFalse(HierarchyQueryPolicy.isSpringboardTarget(foregroundBundleId: "com.apple.SafariViewService"))
    }

    func testSafariViewServiceGatingRequiresIOS26NonSafariForegroundAndRemoteSubtree() {
        // iOS < 26: never query, even with remote elements.
        XCTAssertFalse(HierarchyQueryPolicy.shouldQuerySafariViewService(
            osMajorVersion: 18,
            foregroundBundleId: "dev.tapwing.sampleApp",
            hasRemoteElement: true
        ))
        XCTAssertFalse(HierarchyQueryPolicy.shouldQuerySafariViewService(
            osMajorVersion: 25,
            foregroundBundleId: "dev.tapwing.sampleApp",
            hasRemoteElement: true
        ))

        // iOS 26+ normal screen with zero remote elements: skip 100% of the time.
        XCTAssertFalse(HierarchyQueryPolicy.shouldQuerySafariViewService(
            osMajorVersion: 26,
            foregroundBundleId: "dev.tapwing.sampleApp",
            hasRemoteElement: false
        ))

        // iOS 26+ when foreground IS already SafariViewService: skip duplicate snapshot.
        XCTAssertFalse(HierarchyQueryPolicy.shouldQuerySafariViewService(
            osMajorVersion: 26,
            foregroundBundleId: "com.apple.SafariViewService",
            hasRemoteElement: true
        ))

        // iOS 26+ host app with an embedded remote element (SFSafariViewController / ASWebAuthenticationSession): query!
        XCTAssertTrue(HierarchyQueryPolicy.shouldQuerySafariViewService(
            osMajorVersion: 26,
            foregroundBundleId: "dev.tapwing.sampleApp",
            hasRemoteElement: true
        ))
    }

    // MARK: - Micro-benchmark sanity check

    func testWarmCacheLookupMicroBenchmark() {
        var pidCache = PidBundleIdCache()
        let pids: [Int32] = [101, 202, 303, 404, 505, 606, 707, 808]
        _ = pidCache.resolve(activePids: pids) { "com.example.app\($0)" }

        let clock = ManualClock(1.0)
        var hierarchyCache = HierarchyMetadataCache(statusBarTTL: 3.0, now: { clock.now })
        let frame: AXFrame = ["X": 0, "Y": 0, "Width": 393, "Height": 852]
        let bars = [AXElement(identifier: "sb", label: "9:41", elementType: 42, enabled: true)]
        _ = hierarchyCache.deviceFrame(for: .portrait) { frame }
        _ = hierarchyCache.statusBars(orientation: .portrait, foregroundBundleId: "com.example.app202") { bars }

        let iterations = 10_000
        let start = ProcessInfo.processInfo.systemUptime
        var lookupCount = 0
        for _ in 0..<iterations {
            _ = pidCache.resolve(activePids: pids) { _ in
                lookupCount += 1
                return nil
            }
            _ = hierarchyCache.deviceFrame(for: .portrait) {
                lookupCount += 1
                return frame
            }
            _ = hierarchyCache.statusBars(orientation: .portrait, foregroundBundleId: "com.example.app202") {
                lookupCount += 1
                return bars
            }
        }
        let elapsedMs = (ProcessInfo.processInfo.systemUptime - start) * 1000.0
        let perIterUs = (elapsedMs * 1000.0) / Double(iterations)
        print(String(format: "[RunnerCachesTests] %d warm cache iterations (8 PIDs + frame + statusBars): %.2f ms (%.2f us/iter)", iterations, elapsedMs, perIterUs))
        XCTAssertEqual(lookupCount, 0)
        // Warm cache lookups across 10k iterations should stay well under a few hundred ms.
        XCTAssertLessThan(elapsedMs, 5000.0)
    }
}
