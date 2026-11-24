// Minimal Objective-C bridge for the few in-process XCTest runtime hooks
// required for Maestro 2.12.1 parity (interruption suppression, active app
// enumeration, snapshot parameters, and synthesized pointer/text events).
//
// Semantics derived from Maestro 2.12.1 (`maestro-ios-xctest-runner`,
// Apache-2.0, Copyright (c) 2022 mobile.dev inc.) and WebDriverAgent
// (`XCAXClient_iOS+FBSnapshotReqParams`, BSD-3-Clause, Copyright (c)
// 2015-present Facebook, Inc.), rewritten for Tapwing without dumped
// Apple private header trees.

#import <Foundation/Foundation.h>
#import <UIKit/UIKit.h>
#import <XCTest/XCTest.h>

NS_ASSUME_NONNULL_BEGIN

/// Sets a custom key/value merged into `-[XCAXClient_iOS defaultParameters]`
/// (for example `maxDepth` or `snapshotKeyHonorModalViews`).
void TapwingSetSnapshotParameter(NSString *name, id value);

/// Removes a custom key previously installed via `TapwingSetSnapshotParameter`.
void TapwingRemoveSnapshotParameter(NSString *name);

/// Returns `{ @"pid": @(pid), @"bundleId": bundleId }` for every active
/// application reported by `XCUIDevice.sharedDevice.accessibilityInterface`.
NSArray<NSDictionary<NSString *, id> *> *TapwingActiveAppsInfo(void);

/// Synthesizes a single tap or long-press at `point` (already rotated into the
/// portrait coordinate space expected by `XCPointerEventPath`).
void TapwingSynthesizeTouch(
    CGPoint point,
    NSTimeInterval duration,
    UIInterfaceOrientation orientation,
    void (^completion)(NSError * _Nullable error)
);

/// Synthesizes a swipe gesture on `targetElement`'s `eventTarget`.
/// Returns `NO` and populates `outError` on failure (preserving Apple's
/// `NSError` domain/code for HTTP 408 timeout classification).
BOOL TapwingDispatchSwipe(
    XCUIElement *targetElement,
    NSString *description,
    CGPoint startPoint,
    CGPoint endPoint,
    NSTimeInterval duration,
    UIInterfaceOrientation orientation,
    NSError * _Nullable * _Nullable outError
);

/// Synthesizes text input (`typeText:atOffset:typingSpeed:shouldRedact:`) via
/// `XCTRunnerDaemonSession.sharedSession.daemonProxy`.
void TapwingSynthesizeTextInput(
    NSString *text,
    uint64_t typingSpeed,
    UIInterfaceOrientation orientation,
    void (^completion)(NSError * _Nullable error)
);

/// Presses the Simulator hardware lock button (`-[XCUIDevice pressLockButton]`).
void TapwingPressLockButton(void);

NS_ASSUME_NONNULL_END
