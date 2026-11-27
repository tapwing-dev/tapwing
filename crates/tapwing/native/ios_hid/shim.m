// Tapwing iOS Simulator HID and accessibility bridge (spike for issue #30).
//
// Adapted from agent-phone (https://github.com/govijr/agent-phone, MIT, Copyright (c) 2026
// Govijr, commit 4a355d4, native/shim.m), without its iPhone Mirroring and annotation parts.
// The Indigo HID message layout and the AXPTranslator bridge pattern originate in facebook/idb
// (MIT, Copyright (c) Meta Platforms, Inc. and affiliates). Full license texts are in
// THIRD_PARTY_NOTICES.md.
//
// CoreSimulator, SimulatorKit and AccessibilityPlatformTranslation are private Apple frameworks.
// They are loaded with dlopen at run time and messaged through informal protocols, so nothing
// here links against them. No Objective-C exception leaves a function declared in shim.h.

#import "shim.h"

#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <IOSurface/IOSurface.h>
#import <ImageIO/ImageIO.h>
#import <dlfcn.h>
#import <libproc.h>
#import <mach/mach_time.h>

#pragma mark - Private API declarations

@protocol APSimServiceContext <NSObject>
+ (id)sharedServiceContextForDeveloperDir:(NSString *)dir error:(NSError **)error;
- (id)defaultDeviceSetWithError:(NSError **)error;
@end

@protocol APSimDeviceSet <NSObject>
- (NSArray *)devices;
@end

@protocol APSimDeviceType <NSObject>
- (CGSize)mainScreenSize;
- (float)mainScreenScale;
@end

@protocol APSimDevice <NSObject>
- (NSUUID *)UDID;
- (unsigned long long)state;
- (id)deviceType;
- (void)sendAccessibilityRequestAsync:(id)request
                      completionQueue:(dispatch_queue_t)queue
                    completionHandler:(void (^)(id response))handler;
@end

@protocol APHIDClient <NSObject>
- (instancetype)initWithDevice:(id)device error:(NSError **)error;
- (void)sendWithMessage:(void *)message
           freeWhenDone:(BOOL)freeWhenDone
        completionQueue:(dispatch_queue_t)queue
             completion:(void (^)(NSError *error))completion;
@end

@protocol APTranslationObject <NSObject>
@property (nonatomic, retain) NSString *bridgeDelegateToken;
@property (nonatomic) int pid;
@end

@protocol APTranslator <NSObject>
+ (id)sharedInstance;
@property (nonatomic, weak) id bridgeTokenDelegate;
@property (nonatomic) BOOL supportsDelegateTokens;
@property (nonatomic) BOOL accessibilityEnabled;
- (id)frontmostApplicationWithDisplayId:(unsigned int)displayId bridgeDelegateToken:(NSString *)token;
- (id)translationApplicationObjectForPid:(int)pid;
- (id)objectAtPoint:(CGPoint)point displayId:(unsigned int)displayId bridgeDelegateToken:(NSString *)token;
- (id)macPlatformElementFromTranslation:(id)translation;
@end

@protocol APTranslatorResponse <NSObject>
+ (id)emptyResponse;
@end

@protocol APPlatformElement <NSObject>
- (id)translation;
- (NSArray *)accessibilityChildren;
- (NSString *)accessibilityRole;
- (NSString *)accessibilitySubrole;
- (NSString *)accessibilityLabel;
- (id)accessibilityValue;
- (NSString *)accessibilityIdentifier;
- (NSString *)accessibilityPlaceholderValue;
- (CGRect)accessibilityFrame;
- (BOOL)isAccessibilityEnabled;
- (BOOL)isAccessibilityFocused;
- (BOOL)isAccessibilitySelected;
- (NSArray *)accessibilityAttributeNames;
- (id)accessibilityAttributeValue:(NSString *)name;
@end

#pragma mark - Errors

static char *ap_strdup(NSString *s) { return strdup((s ?: @"").UTF8String); }

static void set_err(char **err, NSString *message) {
  if (err) {
    *err = ap_strdup(message);
  }
}

// Error classes for the host. A message starts with "[unavailable] " when this Mac or simulator
// cannot serve the backend at all (a framework, class, selector or symbol is missing, or the device
// is not found or not booted), and with "[sent] " when an input message had already been handed
// to SimulatorKit, so the event may have reached the device. Other messages are plain call failures.
static const NSString *const kUnavailable = @"[unavailable] ";
static const NSString *const kSent = @"[sent] ";

static void set_unavailable(char **err, NSString *message) {
  set_err(err, [kUnavailable stringByAppendingString:message]);
}

// True from the moment a message goes to SimulatorKit until the next input call starts.
static __thread BOOL tl_dispatched;

// Marks an input failure as "sent" when a message had been dispatched before it.
static int fail_input(char **err) {
  if (tl_dispatched && err && *err) {
    char *old = *err;
    *err = ap_strdup([kSent stringByAppendingString:@(old)]);
    free(old);
  }
  return 1;
}

void ap_free(char *s) { free(s); }

#pragma mark - Frameworks and devices

static NSString *load_frameworks(NSString *devDir) {
  static NSString *failure;
  static dispatch_once_t once;
  dispatch_once(&once, ^{
    NSString *xcodeContents = [devDir stringByDeletingLastPathComponent];
    NSArray<NSArray<NSString *> *> *candidates = @[
      @[ @"/Library/Developer/PrivateFrameworks/CoreSimulator.framework/CoreSimulator" ],
      @[
        [xcodeContents stringByAppendingPathComponent:@"SharedFrameworks/SimulatorKit.framework/SimulatorKit"],
        [devDir stringByAppendingPathComponent:@"Library/PrivateFrameworks/SimulatorKit.framework/SimulatorKit"],
      ],
      @[ @"/System/Library/PrivateFrameworks/AccessibilityPlatformTranslation.framework/AccessibilityPlatformTranslation" ],
    ];
    for (NSArray<NSString *> *paths in candidates) {
      BOOL loaded = NO;
      NSString *lastError = nil;
      for (NSString *path in paths) {
        if (dlopen(path.fileSystemRepresentation, RTLD_NOW | RTLD_GLOBAL)) {
          loaded = YES;
          break;
        }
        const char *e = dlerror();
        lastError = e ? @(e) : path;
      }
      if (!loaded) {
        failure = [NSString stringWithFormat:@"could not load %@: %@", paths.firstObject.lastPathComponent, lastError];
        return;
      }
    }
  });
  return failure;
}

// Devices and HID clients are kept for the life of the process, so repeated calls skip the
// device set lookup and the HID connection setup.
static NSMutableDictionary<NSString *, id> *device_cache(void) {
  static NSMutableDictionary *cache;
  static dispatch_once_t once;
  dispatch_once(&once, ^{ cache = [NSMutableDictionary dictionary]; });
  return cache;
}

static id<APSimDevice> find_device(const char *dev_dir, const char *udid, char **err) {
  NSString *devDir = @(dev_dir);
  NSString *failure = load_frameworks(devDir);
  if (failure) {
    set_unavailable(err, failure);
    return nil;
  }
  NSString *key = @(udid);
  id<APSimDevice> cached = device_cache()[key];
  if (cached) {
    if (cached.state != 3) {
      set_unavailable(err, [NSString stringWithFormat:@"simulator %s is not booted", udid]);
      return nil;
    }
    return cached;
  }
  Class contextClass = NSClassFromString(@"SimServiceContext");
  if (!contextClass) {
    set_unavailable(err, @"CoreSimulator has no SimServiceContext; is Xcode installed and opened once?");
    return nil;
  }
  NSError *error = nil;
  id context = [(id<APSimServiceContext>)contextClass sharedServiceContextForDeveloperDir:devDir error:&error];
  id set = [context defaultDeviceSetWithError:&error];
  if (!set) {
    set_unavailable(err, [NSString stringWithFormat:@"could not open the simulator device set: %@", error.localizedDescription]);
    return nil;
  }
  NSUUID *wanted = [[NSUUID alloc] initWithUUIDString:key];
  for (id<APSimDevice> device in [(id<APSimDeviceSet>)set devices]) {
    if ([device.UDID isEqual:wanted]) {
      if (device.state != 3) {
        set_unavailable(err, [NSString stringWithFormat:@"simulator %s is not booted", udid]);
        return nil;
      }
      device_cache()[key] = device;
      return device;
    }
  }
  set_unavailable(err, [NSString stringWithFormat:@"no simulator with UDID %s", udid]);
  return nil;
}

static CGSize screen_points(id<APSimDevice> device, float *scaleOut) {
  id<APSimDeviceType> type = device.deviceType;
  CGSize pixels = type.mainScreenSize;
  float scale = type.mainScreenScale ?: 1;
  if (scaleOut) {
    *scaleOut = scale;
  }
  return CGSizeMake(pixels.width / scale, pixels.height / scale);
}

char *ap_screen(const char *dev_dir, const char *udid, char **err) {
  @autoreleasepool {
    @try {
      id<APSimDevice> device = find_device(dev_dir, udid, err);
      if (!device) {
        return NULL;
      }
      float scale = 1;
      CGSize size = screen_points(device, &scale);
      return ap_strdup([NSString stringWithFormat:@"{\"width\":%g,\"height\":%g,\"scale\":%g}",
                                                  size.width, size.height, scale]);
    } @catch (NSException *exception) {
      set_err(err, exception.reason);
      return NULL;
    }
  }
}

#pragma mark - HID

typedef void *(*MouseFn)(CGPoint *, CGPoint *, uint32_t, NSUInteger, CGSize, uint32_t);
typedef void *(*ButtonFn)(int32_t, int32_t, int32_t);
typedef void *(*KeyboardFn)(int32_t, int32_t);

static void *simkit_symbol(const char *name) {
  // RTLD_DEFAULT finds the symbol once SimulatorKit has been loaded with RTLD_GLOBAL.
  return dlsym(RTLD_DEFAULT, name);
}

static id<APHIDClient> hid_client(id<APSimDevice> device, NSString *udid, char **err) {
  NSString *key = [@"hid:" stringByAppendingString:udid];
  id<APHIDClient> cached = device_cache()[key];
  if (cached) {
    return cached;
  }
  Class clientClass = NSClassFromString(@"SimulatorKit.SimDeviceLegacyHIDClient");
  if (!clientClass) {
    set_unavailable(err, @"SimulatorKit.SimDeviceLegacyHIDClient is not available in this Xcode");
    return nil;
  }
  NSError *error = nil;
  id<APHIDClient> client = [(id<APHIDClient>)[clientClass alloc] initWithDevice:device error:&error];
  if (!client) {
    set_unavailable(err, [NSString stringWithFormat:@"could not connect to the simulator's HID service: %@",
                                            error.localizedDescription]);
    return nil;
  }
  device_cache()[key] = client;
  return client;
}

// Sends one malloc'd message and waits for the simulator to acknowledge it.
static BOOL hid_send(id<APHIDClient> client, void *message, char **err) {
  static dispatch_queue_t queue;
  static dispatch_once_t once;
  dispatch_once(&once, ^{
    queue = dispatch_queue_create("tapwing.hid", DISPATCH_QUEUE_SERIAL);
  });
  if (!message) {
    set_err(err, @"SimulatorKit returned no HID message");
    return NO;
  }
  dispatch_semaphore_t done = dispatch_semaphore_create(0);
  __block NSError *failure = nil;
  tl_dispatched = YES;
  [client sendWithMessage:message
             freeWhenDone:YES
          completionQueue:queue
               completion:^(NSError *error) {
                 failure = error;
                 dispatch_semaphore_signal(done);
               }];
  if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC)) != 0) {
    set_err(err, @"the simulator did not acknowledge the input within 5s");
    return NO;
  }
  if (failure) {
    set_err(err, [NSString stringWithFormat:@"the simulator rejected the input: %@", failure.localizedDescription]);
    return NO;
  }
  return YES;
}

// A single-touch message. SimulatorKit only builds multi-touch messages, so this takes the touch
// payload from its builder and re-envelopes it as a single-touch message, as idb does.
static void *touch_message(MouseFn mouse, CGPoint ratio, BOOL down) {
  CGPoint point = ratio;
  uint8_t *source = mouse(&point, NULL, 0x32, down ? 1 : 2, CGSizeMake(1, 1), 0);
  if (!source) {
    return NULL;
  }
  memcpy(source + 0x3c, &ratio.x, sizeof(double));
  memcpy(source + 0x44, &ratio.y, sizeof(double));

  const size_t payloadSize = 0x90;
  const size_t messageSize = 0xB0 + payloadSize;
  const size_t touchSize = 0x70;
  uint8_t *message = calloc(1, messageSize);
  uint32_t innerSize = (uint32_t)payloadSize;
  memcpy(message + 0x18, &innerSize, 4);
  message[0x1c] = 2;  // single touch
  uint32_t eventKind = 0xB;
  memcpy(message + 0x20, &eventKind, 4);
  uint64_t timestamp = mach_absolute_time();
  memcpy(message + 0x24, &timestamp, 8);
  memcpy(message + 0x30, source + 0x30, touchSize);
  free(source);

  memcpy(message + 0x20 + payloadSize, message + 0x20, payloadSize);
  uint32_t one = 1, two = 2;
  memcpy(message + 0x20 + payloadSize + 0x10, &one, 4);
  memcpy(message + 0x20 + payloadSize + 0x14, &two, 4);
  return message;
}

int ap_touch(const char *dev_dir, const char *udid, const double *xs, const double *ys, int n,
             int hold_ms, int step_ms, char **err) {
  @autoreleasepool {
    tl_dispatched = NO;
    @try {
      if (n < 1) {
        set_err(err, @"a touch needs at least one point");
        return fail_input(err);
      }
      id<APSimDevice> device = find_device(dev_dir, udid, err);
      if (!device) {
        return fail_input(err);
      }
      MouseFn mouse = (MouseFn)simkit_symbol("IndigoHIDMessageForMouseNSEvent");
      if (!mouse) {
        set_unavailable(err, @"IndigoHIDMessageForMouseNSEvent is missing from SimulatorKit");
        return fail_input(err);
      }
      id<APHIDClient> client = hid_client(device, @(udid), err);
      if (!client) {
        return fail_input(err);
      }
      CGSize size = screen_points(device, NULL);
      for (int i = 0; i < n; i++) {
        CGPoint ratio = CGPointMake(xs[i] / size.width, ys[i] / size.height);
        if (!hid_send(client, touch_message(mouse, ratio, YES), err)) {
          return fail_input(err);
        }
        int wait = i == 0 ? hold_ms : step_ms;
        if (wait > 0) {
          usleep((useconds_t)wait * 1000);
        }
      }
      CGPoint last = CGPointMake(xs[n - 1] / size.width, ys[n - 1] / size.height);
      return hid_send(client, touch_message(mouse, last, NO), err) ? 0 : fail_input(err);
    } @catch (NSException *exception) {
      set_err(err, exception.reason);
      return fail_input(err);
    }
  }
}

int ap_button(const char *dev_dir, const char *udid, int button, char **err) {
  @autoreleasepool {
    tl_dispatched = NO;
    @try {
      id<APSimDevice> device = find_device(dev_dir, udid, err);
      if (!device) {
        return fail_input(err);
      }
      ButtonFn make = (ButtonFn)simkit_symbol("IndigoHIDMessageForButton");
      if (!make) {
        set_unavailable(err, @"IndigoHIDMessageForButton is missing from SimulatorKit");
        return fail_input(err);
      }
      id<APHIDClient> client = hid_client(device, @(udid), err);
      if (!client) {
        return fail_input(err);
      }
      // idb uses target 0x33. On Xcode 27.2 with iOS 27.2 only 0x32 moved the simulator (home 3 of 3, lock 1 of 1
      // plus wake worked), 0x33 had no effect. Other values may crash backboardd, so do not
      // try them. TAPWING_HID_BUTTON_TARGET overrides the default for experiments.
      int32_t source = button == 1 ? 0x1 : 0x0;
      const char *override = getenv("TAPWING_HID_BUTTON_TARGET");
      int32_t target = override ? (int32_t)strtol(override, NULL, 0) : 0x32;
      if (!hid_send(client, make(source, 1, target), err)) {
        return fail_input(err);
      }
      usleep(80 * 1000);
      return hid_send(client, make(source, 2, target), err) ? 0 : fail_input(err);
    } @catch (NSException *exception) {
      set_err(err, exception.reason);
      return fail_input(err);
    }
  }
}

int ap_keys(const char *dev_dir, const char *udid, const uint32_t *usages,
            const uint8_t *directions, int n, char **err) {
  @autoreleasepool {
    tl_dispatched = NO;
    @try {
      id<APSimDevice> device = find_device(dev_dir, udid, err);
      if (!device) {
        return fail_input(err);
      }
      KeyboardFn make = (KeyboardFn)simkit_symbol("IndigoHIDMessageForKeyboardArbitrary");
      if (!make) {
        set_unavailable(err, @"IndigoHIDMessageForKeyboardArbitrary is missing from SimulatorKit");
        return fail_input(err);
      }
      id<APHIDClient> client = hid_client(device, @(udid), err);
      if (!client) {
        return fail_input(err);
      }
      for (int i = 0; i < n; i++) {
        if (!hid_send(client, make((int32_t)usages[i], directions[i]), err)) {
          return fail_input(err);
        }
        usleep(4 * 1000);
      }
      return 0;
    } @catch (NSException *exception) {
      set_err(err, exception.reason);
      return fail_input(err);
    }
  }
}

#pragma mark - Accessibility

// Answers AXPTranslator's requests for element data by forwarding them to the simulator through
// CoreSimulator and waiting for the reply.
@interface APTranslationBridge : NSObject
@property (nonatomic, strong) id<APSimDevice> device;
@property (nonatomic, assign) double timeout;
@property (nonatomic, strong) dispatch_queue_t replyQueue;
@end

@implementation APTranslationBridge

- (id (^)(id))callback {
  id<APSimDevice> device = self.device;
  dispatch_queue_t queue = self.replyQueue;
  double timeout = self.timeout;
  return ^id(id request) {
    dispatch_semaphore_t done = dispatch_semaphore_create(0);
    __block id response = nil;
    [device sendAccessibilityRequestAsync:request
                          completionQueue:queue
                        completionHandler:^(id reply) {
                          response = reply;
                          dispatch_semaphore_signal(done);
                        }];
    if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(timeout * NSEC_PER_SEC))) != 0) {
      return [(id<APTranslatorResponse>)NSClassFromString(@"AXPTranslatorResponse") emptyResponse];
    }
    return response;
  };
}

- (id)accessibilityTranslationDelegateBridgeCallbackWithToken:(NSString *)token { return [self callback]; }
- (id)accessibilityTranslationDelegateBridgeCallback { return [self callback]; }
- (CGRect)accessibilityTranslationConvertPlatformFrameToSystem:(CGRect)rect withToken:(NSString *)token { return rect; }
- (CGRect)accessibilityTranslationConvertPlatformFrameToSystem:(CGRect)rect withContext:(id)c postProcess:(id)p { return rect; }
- (id)accessibilityTranslationRootParentWithToken:(NSString *)token { return nil; }
- (id)accessibilityTranslationRootParent { return nil; }

@end

static NSString *const kToken = @"tapwing-hid";

// The display id the translator is asked about. 0 is the main display; TAPWING_HID_DISPLAY_ID overrides it for experiments.
static unsigned int display_id(void) {
  const char *v = getenv("TAPWING_HID_DISPLAY_ID");
  return v ? (unsigned int)strtoul(v, NULL, 0) : 0;
}

static id string_or_null(id value) {
  if ([value isKindOfClass:NSString.class]) {
    return [(NSString *)value length] ? value : NSNull.null;
  }
  if ([value isKindOfClass:NSNumber.class]) {
    return [(NSNumber *)value stringValue];
  }
  if ([value isKindOfClass:NSAttributedString.class]) {
    return [(NSAttributedString *)value string];
  }
  return NSNull.null;
}

static void tag(id element) {
  @try {
    id translation = [(id<APPlatformElement>)element translation];
    [(id<APTranslationObject>)translation setBridgeDelegateToken:kToken];
  } @catch (NSException *exception) {
  }
}

// One attribute by its Mac accessibility name, or nil when the element does not answer.
static id attribute(id<APPlatformElement> element, NSString *name) {
  @try {
    return [element accessibilityAttributeValue:name];
  } @catch (NSException *exception) {
    return nil;
  }
}

static id attribute_json(id value) {
  if (!value || value == NSNull.null) {
    return NSNull.null;
  }
  if ([value isKindOfClass:NSString.class] || [value isKindOfClass:NSNumber.class]) {
    return value;
  }
  if ([value isKindOfClass:NSAttributedString.class]) {
    return [(NSAttributedString *)value string];
  }
  if ([value isKindOfClass:NSArray.class]) {
    NSMutableArray *out = [NSMutableArray array];
    for (id item in (NSArray *)value) {
      if ([item isKindOfClass:NSString.class] || [item isKindOfClass:NSNumber.class]) {
        [out addObject:item];
      } else {
        [out addObject:[NSString stringWithFormat:@"<%@>", NSStringFromClass([item class])]];
      }
    }
    return out;
  }
  if ([value isKindOfClass:NSValue.class]) {
    return [NSString stringWithFormat:@"%@", value];
  }
  return [NSString stringWithFormat:@"<%@>", NSStringFromClass([value class])];
}

typedef struct {
  int all_attrs;
  int max_depth;
  CFAbsoluteTime deadline;
  BOOL truncated;
} walk_state;

static NSDictionary *describe(id<APPlatformElement> element, int depth, walk_state *st) {
  tag(element);
  NSMutableDictionary *node = [NSMutableDictionary dictionary];
  node[@"role"] = string_or_null(element.accessibilityRole);
  node[@"subrole"] = string_or_null(element.accessibilitySubrole);
  node[@"roleDescription"] = string_or_null(attribute(element, @"AXRoleDescription"));
  node[@"label"] = string_or_null(element.accessibilityLabel);
  node[@"title"] = string_or_null(attribute(element, @"AXTitle"));
  node[@"value"] = string_or_null(element.accessibilityValue);
  node[@"identifier"] = string_or_null(element.accessibilityIdentifier);
  node[@"placeholder"] = string_or_null(
      [element respondsToSelector:@selector(accessibilityPlaceholderValue)] ? element.accessibilityPlaceholderValue : nil);
  node[@"help"] = string_or_null(attribute(element, @"AXHelp"));
  node[@"enabled"] = @([element respondsToSelector:@selector(isAccessibilityEnabled)] ? element.isAccessibilityEnabled : YES);
  node[@"focused"] = @([element respondsToSelector:@selector(isAccessibilityFocused)] ? element.isAccessibilityFocused : NO);
  node[@"selected"] = @([element respondsToSelector:@selector(isAccessibilitySelected)] ? element.isAccessibilitySelected : NO);
  CGRect frame = element.accessibilityFrame;
  node[@"frame"] = @[ @(frame.origin.x), @(frame.origin.y), @(frame.size.width), @(frame.size.height) ];
  if (st->all_attrs) {
    NSMutableDictionary *attrs = [NSMutableDictionary dictionary];
    NSArray *names = @[];
    @try {
      names = [element accessibilityAttributeNames] ?: @[];
    } @catch (NSException *exception) {
    }
    NSSet *skip = [NSSet setWithArray:@[ @"AXChildren", @"AXParent", @"AXWindow", @"AXTopLevelUIElement",
                                         @"AXVisibleChildren", @"AXChildrenInNavigationOrder" ]];
    for (NSString *name in names) {
      if (![skip containsObject:name]) {
        attrs[name] = attribute_json(attribute(element, name));
      }
    }
    node[@"attrs"] = attrs;
  }
  NSMutableArray *children = [NSMutableArray array];
  if (depth < st->max_depth) {
    if (CFAbsoluteTimeGetCurrent() > st->deadline) {
      st->truncated = YES;
    } else {
      NSArray *kids = nil;
      @try {
        kids = element.accessibilityChildren;
      } @catch (NSException *exception) {
      }
      for (id child in kids ?: @[]) {
        [children addObject:describe(child, depth + 1, st)];
      }
    }
  }
  node[@"children"] = children;
  return node;
}

static NSDictionary *describe_root(id<APTranslator> translator, id translation, walk_state *st, NSString **failure) {
  [(id<APTranslationObject>)translation setBridgeDelegateToken:kToken];
  id root = [translator macPlatformElementFromTranslation:translation];
  if (!root) {
    *failure = @"could not read the accessibility element of the application";
    return nil;
  }
  NSMutableDictionary *tree = [describe(root, 0, st) mutableCopy];
  int pid = [(id<APTranslationObject>)translation pid];
  tree[@"pid"] = @(pid);
  // The executable name of the process, so the host can tell SpringBoard from an app without
  // knowing any pid. Simulator processes are ordinary host processes, so this works for them.
  char path[PROC_PIDPATHINFO_MAXSIZE];
  if (pid > 0 && proc_pidpath(pid, path, sizeof path) > 0) {
    tree[@"process"] = [@(path) lastPathComponent];
  }
  return tree;
}

// Everything the worker needs, kept alive by the block so a late worker stays safe.
@interface APSnapshotJob : NSObject
@property (nonatomic) ap_snapshot_opts opts;
@property (nonatomic) double timeout;
@property (nonatomic) CGSize screen;
@property (nonatomic, strong) APTranslationBridge *bridge;
@property (nonatomic, copy) NSString *failure;
@property (nonatomic, copy) NSData *json;
@end

@implementation APSnapshotJob
- (void)run {
  Class translatorClass = NSClassFromString(@"AXPTranslator");
  ap_snapshot_opts o = self.opts;
  id<APTranslator> translator = [(id<APTranslator>)translatorClass sharedInstance];
  translator.bridgeTokenDelegate = self.bridge;
  if ([translator respondsToSelector:@selector(setSupportsDelegateTokens:)]) {
    translator.supportsDelegateTokens = YES;
  }
  if (o.enable_ax && [translator respondsToSelector:@selector(setAccessibilityEnabled:)]) {
    translator.accessibilityEnabled = YES;
  }
  walk_state st = {o.all_attrs, o.max_depth > 0 ? o.max_depth : 64, CFAbsoluteTimeGetCurrent() + self.timeout * 5, NO};
  id first = nil;
  if (o.has_point) {
    first = [translator objectAtPoint:CGPointMake(o.x, o.y) displayId:display_id() bridgeDelegateToken:kToken];
  } else if (o.pid > 0) {
    first = [translator translationApplicationObjectForPid:o.pid];
  } else {
    first = [translator frontmostApplicationWithDisplayId:display_id() bridgeDelegateToken:kToken];
    if (!first) {
      // The frontmost request sometimes gets no answer (seen with the keyboard up, and with
      // accessibility just switched on) while hit tests still work. Hit test a column of points
      // and take the process of the first answer.
      // With the accessibility flags at 0 only some elements answer a hit test (a SpringBoard alert
      // does, but not the gaps between its parts), so the column is dense around the middle.
      static const double fractions[] = {0.3, 0.15, 0.5, 0.7, 0.9, 0.36, 0.4, 0.45, 0.55, 0.58, 0.6, 0.65};
      for (int k = 0; k < (int)(sizeof fractions / sizeof fractions[0]) && !first; k++) {
        id hit = [translator objectAtPoint:CGPointMake(self.screen.width / 2, self.screen.height * fractions[k])
                                  displayId:display_id()
                        bridgeDelegateToken:kToken];
        int pid = hit ? [(id<APTranslationObject>)hit pid] : 0;
        first = pid > 0 ? [translator translationApplicationObjectForPid:pid] : nil;
      }
    }
  }
  if (!first) {
    self.failure = o.has_point ? @"no element at that point"
                   : o.pid > 0 ? @"no application with that pid"
                               : @"no frontmost application; is the simulator still starting up?";
    return;
  }
  NSString *failure = nil;
  NSMutableArray *roots = [NSMutableArray array];
  NSDictionary *tree = describe_root(translator, first, &st, &failure);
  if (!tree) {
    self.failure = failure;
    return;
  }
  [roots addObject:tree];
  int firstPid = [tree[@"pid"] intValue];
  int extra = o.springboard;
  if (extra < 0) {
    // SpringBoard owns the status bar: hit test along the top edge until a pid shows up that
    // differs from the first root's.
    for (double fx = 0.25; fx < 0.95 && extra <= 0; fx += 0.25) {
      id hit = [translator objectAtPoint:CGPointMake(self.screen.width * fx, 14) displayId:display_id() bridgeDelegateToken:kToken];
      int pid = hit ? [(id<APTranslationObject>)hit pid] : 0;
      if (pid > 0 && pid != firstPid) {
        extra = pid;
      }
    }
  }
  if (extra > 0 && extra != firstPid) {
    id sb = [translator translationApplicationObjectForPid:extra];
    NSDictionary *sbTree = sb ? describe_root(translator, sb, &st, &failure) : nil;
    if (sbTree) {
      [roots addObject:sbTree];
    }
  }
  NSMutableDictionary *doc = [@{@"roots" : roots} mutableCopy];
  if (st.truncated) {
    doc[@"truncated"] = @YES;
  }
  self.json = [NSJSONSerialization dataWithJSONObject:doc options:0 error:nil];
  if (!self.json) {
    self.failure = @"could not encode the accessibility tree";
  }
}
@end

char *ap_snapshot(const char *dev_dir, const char *udid, double timeout_s,
                  const ap_snapshot_opts *opts, char **err) {
  @autoreleasepool {
    @try {
      id<APSimDevice> device = find_device(dev_dir, udid, err);
      if (!device) {
        return NULL;
      }
      if (![(id)device respondsToSelector:@selector(sendAccessibilityRequestAsync:completionQueue:completionHandler:)]) {
        set_unavailable(err, @"this CoreSimulator has no accessibility request API");
        return NULL;
      }
      if (!NSClassFromString(@"AXPTranslator")) {
        set_unavailable(err, @"AXPTranslator is not available on this Mac");
        return NULL;
      }
      double timeout = timeout_s > 0 ? timeout_s : 5;

      static APTranslationBridge *bridge;  // the translator only holds it weakly
      bridge = [APTranslationBridge new];
      bridge.device = device;
      bridge.timeout = timeout;
      bridge.replyQueue = dispatch_queue_create("tapwing.ax.reply", DISPATCH_QUEUE_SERIAL);

      APSnapshotJob *job = [APSnapshotJob new];
      job.opts = opts ? *opts : (ap_snapshot_opts){0};
      job.timeout = timeout;
      job.screen = screen_points(device, NULL);
      job.bridge = bridge;

      // The translator asserts it is not on the main queue, and its callbacks block, so walk the
      // tree on a worker queue and wait here for at most 6 times the request timeout. A walk that
      // outlives the wait keeps running on its queue and the caller gets an error.
      dispatch_semaphore_t done = dispatch_semaphore_create(0);
      dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
        @autoreleasepool {
          @try {
            [job run];
          } @catch (NSException *exception) {
            job.failure = [NSString stringWithFormat:@"accessibility read failed: %@", exception.reason];
          }
        }
        dispatch_semaphore_signal(done);
      });
      if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, (int64_t)(timeout * 6 * NSEC_PER_SEC))) != 0) {
        set_err(err, @"the accessibility read did not finish in time");
        return NULL;
      }
      if (!job.json) {
        set_err(err, job.failure ?: @"accessibility read failed");
        return NULL;
      }
      char *out = malloc(job.json.length + 1);
      memcpy(out, job.json.bytes, job.json.length);
      out[job.json.length] = 0;
      return out;
    } @catch (NSException *exception) {
      set_err(err, exception.reason);
      return NULL;
    }
  }
}

#pragma mark - Framebuffer

// The main display's framebuffer, from the device's IO ports (as Simulator.app shows it).
static IOSurfaceRef main_surface(id<APSimDevice> device, char **err) {
  id io = [(id)device respondsToSelector:@selector(io)] ? [(id)device performSelector:@selector(io)] : nil;
  NSArray *ports = [io respondsToSelector:@selector(ioPorts)] ? [io performSelector:@selector(ioPorts)] : nil;
  IOSurfaceRef best = NULL;
  size_t bestArea = 0;
  for (id port in ports ?: @[]) {
    id descriptor = [port respondsToSelector:@selector(descriptor)] ? [port performSelector:@selector(descriptor)] : nil;
    for (NSString *name in @[ @"framebufferSurface", @"ioSurface" ]) {
      SEL sel = NSSelectorFromString(name);
      if (![descriptor respondsToSelector:sel]) {
        continue;
      }
      IOSurfaceRef (*getter)(id, SEL) = (IOSurfaceRef(*)(id, SEL))[descriptor methodForSelector:sel];
      IOSurfaceRef surface = getter(descriptor, sel);
      if (!surface) {
        continue;
      }
      size_t area = IOSurfaceGetWidth(surface) * IOSurfaceGetHeight(surface);
      if (area > bestArea) {
        best = surface;
        bestArea = area;
      }
    }
  }
  if (!best) {
    set_err(err, @"the simulator exposes no display surface");
  }
  return best;
}

char *ap_frame(const char *dev_dir, const char *udid, double quality, size_t *len, char **err) {
  @autoreleasepool {
    @try {
      id<APSimDevice> device = find_device(dev_dir, udid, err);
      if (!device) {
        return NULL;
      }
      IOSurfaceRef surface = main_surface(device, err);
      if (!surface) {
        return NULL;
      }
      CFRetain(surface);
      IOSurfaceLock(surface, kIOSurfaceLockReadOnly, NULL);
      size_t w = IOSurfaceGetWidth(surface), h = IOSurfaceGetHeight(surface), row = IOSurfaceGetBytesPerRow(surface);
      CGColorSpaceRef space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
      CGContextRef ctx = CGBitmapContextCreate(IOSurfaceGetBaseAddress(surface), w, h, 8, row, space,
                                               kCGBitmapByteOrder32Little | (CGBitmapInfo)kCGImageAlphaPremultipliedFirst);
      CGImageRef image = ctx ? CGBitmapContextCreateImage(ctx) : NULL;
      if (ctx) {
        CGContextRelease(ctx);
      }
      CGColorSpaceRelease(space);
      IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL);
      CFRelease(surface);
      if (!image) {
        set_err(err, @"could not read the display surface");
        return NULL;
      }
      NSMutableData *data = [NSMutableData data];
      CFStringRef type = quality > 0 ? CFSTR("public.jpeg") : CFSTR("public.png");
      CGImageDestinationRef dest = CGImageDestinationCreateWithData((__bridge CFMutableDataRef)data, type, 1, NULL);
      NSDictionary *props = quality > 0 ? @{(id)kCGImageDestinationLossyCompressionQuality : @(quality)} : @{};
      CGImageDestinationAddImage(dest, image, (__bridge CFDictionaryRef)props);
      BOOL ok = CGImageDestinationFinalize(dest);
      CFRelease(dest);
      CGImageRelease(image);
      if (!ok) {
        set_err(err, @"could not encode the frame");
        return NULL;
      }
      char *out = malloc(data.length);
      memcpy(out, data.bytes, data.length);
      *len = data.length;
      return out;
    } @catch (NSException *exception) {
      set_err(err, exception.reason);
      return NULL;
    }
  }
}
