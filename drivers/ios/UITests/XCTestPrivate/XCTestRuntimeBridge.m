// Minimal Objective-C runtime bridge for Tapwing's iOS Simulator XCUITest runner.
//
// Semantics ported from Maestro 2.12.1 (`XCUIApplication+Helper.m`,
// `AXClientProxy.m`, `XCTestDaemonsProxy.m`, `EventRecord.swift`,
// `PointerEventPath.swift`, `EventTarget.swift`, `RunnerDaemonProxy.swift`,
// `AXClientSwizzler.swift`, Apache-2.0, Copyright (c) 2022 mobile.dev inc.)
// and WebDriverAgent (`XCAXClient_iOS+FBSnapshotReqParams.m`, BSD-3-Clause,
// Copyright (c) 2015-present Facebook, Inc.), rewritten for Tapwing.

#import "XCTestRuntimeBridge.h"
#import <objc/runtime.h>

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wundeclared-selector"

// Minimal forward declarations of in-process XCTest selectors so ARC manages
// `alloc`/`init` and block arguments safely without raw `unsafeBitCast`.
@interface NSObject (TapwingXCTestPrivate)
- (id)accessibilityInterface;
- (NSArray *)activeApplications;
- (int)processIdentifier;
+ (instancetype)sharedSession;
- (id)daemonProxy;
- (void)_XCT_requestBundleIDForPID:(pid_t)pid reply:(void (^)(NSString * _Nullable bundleID, NSError * _Nullable error))reply;
- (void)_XCT_synthesizeEvent:(id)event completion:(void (^)(NSError * _Nullable error))completion;
- (instancetype)initWithName:(NSString *)name interfaceOrientation:(UIInterfaceOrientation)orientation;
- (void)addPointerEventPath:(id)path;
- (instancetype)initForTouchAtPoint:(CGPoint)point offset:(NSTimeInterval)offset;
- (instancetype)initForTextInput;
- (void)liftUpAtOffset:(NSTimeInterval)offset;
- (void)moveToPoint:(CGPoint)point atOffset:(NSTimeInterval)offset;
- (void)typeText:(NSString *)text atOffset:(NSTimeInterval)offset typingSpeed:(uint64_t)typingSpeed shouldRedact:(BOOL)shouldRedact;
- (id)eventTarget;
- (BOOL)dispatchEventWithDescription:(NSString *)description eventBuilder:(id (^)(void))builder error:(NSError * _Nullable * _Nullable)error;
- (void)pressLockButton;
@end

// MARK: - Snapshot parameter swizzling (single unified hook for maxDepth & snapshotKeyHonorModalViews)

static id (*sOriginalDefaultParameters)(id, SEL) = NULL;
static id (*sOriginalSnapshotParameters)(id, SEL) = NULL;
static NSDictionary *sBaseDefaultParameters = nil;
static NSDictionary *sAdditionalSnapshotParameters = nil;
static NSMutableDictionary<NSString *, id> *sCustomSnapshotParameters = nil;

static id TapwingSwizzledDefaultParameters(id self, SEL _cmd) {
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        sBaseDefaultParameters = sOriginalDefaultParameters ? sOriginalDefaultParameters(self, _cmd) : @{};
    });
    NSMutableDictionary *result = [NSMutableDictionary dictionaryWithDictionary:sBaseDefaultParameters ?: @{}];
    @synchronized([NSProcessInfo processInfo]) {
        if (sAdditionalSnapshotParameters != nil) {
            [result addEntriesFromDictionary:sAdditionalSnapshotParameters];
        }
        if (sCustomSnapshotParameters != nil) {
            [result addEntriesFromDictionary:sCustomSnapshotParameters];
        }
    }
    return [result copy];
}

static id TapwingSwizzledSnapshotParameters(id self, SEL _cmd) {
    NSDictionary *result = sOriginalSnapshotParameters ? sOriginalSnapshotParameters(self, _cmd) : @{};
    @synchronized([NSProcessInfo processInfo]) {
        sAdditionalSnapshotParameters = result;
    }
    return result;
}

static void TapwingInstallSnapshotParameterSwizzleOnce(BOOL alsoSwizzleElementQuery) {
    static dispatch_once_t defaultToken;
    dispatch_once(&defaultToken, ^{
        Class axClientClass = NSClassFromString(@"XCAXClient_iOS");
        if (axClientClass == nil) {
            NSLog(@"[TapwingRunner] XCAXClient_iOS not found; cannot swizzle defaultParameters");
            return;
        }
        Method orig = class_getInstanceMethod(axClientClass, @selector(defaultParameters));
        if (orig == NULL) {
            NSLog(@"[TapwingRunner] -[XCAXClient_iOS defaultParameters] not found");
            return;
        }
        sOriginalDefaultParameters = (id (*)(id, SEL))method_setImplementation(orig, (IMP)TapwingSwizzledDefaultParameters);
    });

    if (alsoSwizzleElementQuery) {
        static dispatch_once_t queryToken;
        dispatch_once(&queryToken, ^{
            Class queryClass = NSClassFromString(@"XCTElementQuery");
            if (queryClass == nil) {
                NSLog(@"[TapwingRunner] XCTElementQuery not found; cannot swizzle snapshotParameters");
                return;
            }
            Method orig = class_getInstanceMethod(queryClass, @selector(snapshotParameters));
            if (orig == NULL) {
                NSLog(@"[TapwingRunner] -[XCTElementQuery snapshotParameters] not found");
                return;
            }
            sOriginalSnapshotParameters = (id (*)(id, SEL))method_setImplementation(orig, (IMP)TapwingSwizzledSnapshotParameters);
        });
    }
}

void TapwingSetSnapshotParameter(NSString *name, id value) {
    TapwingInstallSnapshotParameterSwizzleOnce(NO);
    @synchronized([NSProcessInfo processInfo]) {
        if (sCustomSnapshotParameters == nil) {
            sCustomSnapshotParameters = [NSMutableDictionary new];
        }
        sCustomSnapshotParameters[name] = value;
    }
}

void TapwingRemoveSnapshotParameter(NSString *name) {
    @synchronized([NSProcessInfo processInfo]) {
        [sCustomSnapshotParameters removeObjectForKey:name];
    }
}

// MARK: - Interruption suppression & bootstrap (+load)

@interface XCUIApplication (TapwingInterruptionGuard)
@end

@implementation XCUIApplication (TapwingInterruptionGuard)

- (BOOL)tapwing_doesNotHandleUIInterruptions {
    // Prevent XCTest from auto-dismissing SpringBoard permission alerts so user
    // flows can inspect and tap alert buttons explicitly.
    return YES;
}

+ (void)load {
    // 1. Suppress XCTest automatic UI interruption dismissal.
    SEL origSel = @selector(doesNotHandleUIInterruptions);
    SEL swizSel = @selector(tapwing_doesNotHandleUIInterruptions);
    Method origMethod = class_getInstanceMethod(self, origSel);
    Method swizMethod = class_getInstanceMethod(self, swizSel);
    if (origMethod != NULL && swizMethod != NULL) {
        BOOL added = class_addMethod(
            self,
            origSel,
            method_getImplementation(swizMethod),
            method_getTypeEncoding(swizMethod)
        );
        if (added) {
            class_replaceMethod(
                self,
                swizSel,
                method_getImplementation(origMethod),
                method_getTypeEncoding(origMethod)
            );
        } else {
            method_exchangeImplementations(origMethod, swizMethod);
        }
    } else {
        NSLog(@"[TapwingRunner] Could not swizzle -[XCUIApplication doesNotHandleUIInterruptions]");
    }

    // 2. Honor `TEST_RUNNER_snapshotKeyHonorModalViews=false` (passed by Tapwing
    //    as `snapshotKeyHonorModalViews` inside the runner environment).
    NSString *honorModals = [[[NSProcessInfo processInfo] environment] objectForKey:@"snapshotKeyHonorModalViews"];
    if ([honorModals isEqualToString:@"false"]) {
        NSLog(@"[TapwingRunner] Disabling snapshotKeyHonorModalViews");
        TapwingInstallSnapshotParameterSwizzleOnce(YES);
        TapwingSetSnapshotParameter(@"snapshotKeyHonorModalViews", @0);
    }
}

@end

// MARK: - Daemon & AXClient helpers

static id TapwingSharedDaemonProxy(void) {
    static id proxy = nil;
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        Class sessionClass = NSClassFromString(@"XCTRunnerDaemonSession");
        if (sessionClass != nil) {
            id session = [sessionClass sharedSession];
            proxy = [session daemonProxy];
        }
    });
    return proxy;
}

static id TapwingSharedAXClient(void) {
    static id axClient = nil;
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        axClient = [[XCUIDevice sharedDevice] accessibilityInterface];
    });
    return axClient;
}

static NSError *TapwingMissingPrivateClassError(NSString *className) {
    return [NSError errorWithDomain:@"dev.tapwing.ios-runner"
                               code:1
                           userInfo:@{NSLocalizedDescriptionKey: [NSString stringWithFormat:@"XCTest private class %@ is unavailable", className]}];
}

static NSError *TapwingExceptionError(NSException *exception) {
    NSString *reason = exception.reason ?: exception.name ?: @"Objective-C exception";
    return [NSError errorWithDomain:@"dev.tapwing.ios-runner"
                               code:2
                           userInfo:@{NSLocalizedDescriptionKey: [NSString stringWithFormat:@"%@: %@", exception.name ?: @"NSException", reason]}];
}

NSArray<NSDictionary<NSString *, id> *> *TapwingActiveAppsInfo(void) {
    @try {
        id axClient = TapwingSharedAXClient();
        id daemonProxy = TapwingSharedDaemonProxy();
        if (axClient == nil || daemonProxy == nil) {
            return @[];
        }
        NSArray *axElements = [axClient activeApplications];
        NSMutableArray<NSDictionary<NSString *, id> *> *result = [NSMutableArray arrayWithCapacity:axElements.count];
        for (id axElement in axElements) {
            if (![axElement respondsToSelector:@selector(processIdentifier)]) {
                continue;
            }
            pid_t pid = [axElement processIdentifier];
            // Share mutable state via a locked box so a late XPC reply after
            // semaphore timeout can never race with stack/ARC teardown.
            NSMutableDictionary<NSString *, NSString *> *box = [NSMutableDictionary new];
            dispatch_semaphore_t sem = dispatch_semaphore_create(0);
            [daemonProxy _XCT_requestBundleIDForPID:pid reply:^(NSString * _Nullable resolved, NSError * _Nullable error) {
                if (error == nil && resolved != nil) {
                    @synchronized(box) {
                        box[@"bundleId"] = resolved;
                    }
                }
                dispatch_semaphore_signal(sem);
            }];
            dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(1 * NSEC_PER_SEC)));
            NSString *bundleId = nil;
            @synchronized(box) {
                bundleId = box[@"bundleId"];
            }
            [result addObject:@{
                @"pid": @(pid),
                @"bundleId": bundleId ?: @"unknownBundleId",
            }];
        }
        return [result copy];
    } @catch (NSException *ex) {
        NSLog(@"[TapwingRunner] TapwingActiveAppsInfo threw %@: %@", ex.name, ex.reason);
        return @[];
    }
}

// MARK: - Event synthesis

void TapwingSynthesizeTouch(
    CGPoint point,
    NSTimeInterval duration,
    UIInterfaceOrientation orientation,
    void (^completion)(NSError * _Nullable error)
) {
    @try {
        Class recordClass = NSClassFromString(@"XCSynthesizedEventRecord");
        Class pathClass = NSClassFromString(@"XCPointerEventPath");
        id daemonProxy = TapwingSharedDaemonProxy();
        if (recordClass == nil) { completion(TapwingMissingPrivateClassError(@"XCSynthesizedEventRecord")); return; }
        if (pathClass == nil) { completion(TapwingMissingPrivateClassError(@"XCPointerEventPath")); return; }
        if (daemonProxy == nil) { completion(TapwingMissingPrivateClassError(@"XCTRunnerDaemonSession")); return; }

        NSTimeInterval safeDuration = duration < 0 ? 0 : duration;
        id path = [[pathClass alloc] initForTouchAtPoint:point offset:0.0];
        [path liftUpAtOffset:safeDuration];

        id record = [[recordClass alloc] initWithName:@"Single-Finger Touch Action" interfaceOrientation:orientation];
        [record addPointerEventPath:path];

        [daemonProxy _XCT_synthesizeEvent:record completion:completion];
    } @catch (NSException *ex) {
        completion(TapwingExceptionError(ex));
    }
}

BOOL TapwingDispatchSwipe(
    XCUIElement *targetElement,
    NSString *description,
    CGPoint startPoint,
    CGPoint endPoint,
    NSTimeInterval duration,
    UIInterfaceOrientation orientation,
    NSError * _Nullable * _Nullable outError
) {
    @try {
        Class recordClass = NSClassFromString(@"XCSynthesizedEventRecord");
        Class pathClass = NSClassFromString(@"XCPointerEventPath");
        if (recordClass == nil || pathClass == nil) {
            if (outError != NULL) {
                *outError = TapwingMissingPrivateClassError(recordClass == nil ? @"XCSynthesizedEventRecord" : @"XCPointerEventPath");
            }
            return NO;
        }

        id eventTarget = [targetElement eventTarget];
        if (eventTarget == nil) {
            if (outError != NULL) {
                *outError = TapwingMissingPrivateClassError(@"XCUIElement.eventTarget");
            }
            return NO;
        }

        NSTimeInterval safeDuration = duration < 0 ? 0 : duration;
        NSError *localError = nil;
        BOOL ok = [eventTarget dispatchEventWithDescription:description
                                               eventBuilder:^id{
            id path = [[pathClass alloc] initForTouchAtPoint:startPoint offset:0.0];
            NSTimeInterval offset = 0.1;
            [path moveToPoint:endPoint atOffset:offset];
            offset += safeDuration;
            [path liftUpAtOffset:offset];
            id record = [[recordClass alloc] initWithName:@"Single-Finger Touch Action" interfaceOrientation:orientation];
            [record addPointerEventPath:path];
            return record;
        }
                                                      error:&localError];
        if (localError != nil) {
            if (outError != NULL) {
                *outError = localError;
            }
            return NO;
        }
        return ok;
    } @catch (NSException *ex) {
        if (outError != NULL) {
            *outError = TapwingExceptionError(ex);
        }
        return NO;
    }
}

void TapwingSynthesizeTextInput(
    NSString *text,
    uint64_t typingSpeed,
    UIInterfaceOrientation orientation,
    void (^completion)(NSError * _Nullable error)
) {
    @try {
        Class recordClass = NSClassFromString(@"XCSynthesizedEventRecord");
        Class pathClass = NSClassFromString(@"XCPointerEventPath");
        id daemonProxy = TapwingSharedDaemonProxy();
        if (recordClass == nil) { completion(TapwingMissingPrivateClassError(@"XCSynthesizedEventRecord")); return; }
        if (pathClass == nil) { completion(TapwingMissingPrivateClassError(@"XCPointerEventPath")); return; }
        if (daemonProxy == nil) { completion(TapwingMissingPrivateClassError(@"XCTRunnerDaemonSession")); return; }

        id path = [[pathClass alloc] initForTextInput];
        [path typeText:text atOffset:0.0 typingSpeed:typingSpeed shouldRedact:NO];

        id record = [[recordClass alloc] initWithName:@"Single-Finger Touch Action" interfaceOrientation:orientation];
        [record addPointerEventPath:path];

        [daemonProxy _XCT_synthesizeEvent:record completion:completion];
    } @catch (NSException *ex) {
        completion(TapwingExceptionError(ex));
    }
}

void TapwingPressLockButton(void) {
    XCUIDevice *device = [XCUIDevice sharedDevice];
    if ([device respondsToSelector:@selector(pressLockButton)]) {
        [device pressLockButton];
    }
}

#pragma clang diagnostic pop
