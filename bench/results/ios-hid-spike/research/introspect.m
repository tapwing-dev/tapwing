// Inspects the private API that crates/tapwing/native/ios_hid/shim.m uses (issue #30, question 4).
//
// It only dlopens the three frameworks from the paths the shim uses and reads the Objective-C
// runtime and the symbol table. It does not call SimServiceContext, open a device set, create a
// HID client, send an accessibility request or talk to any simulator.
//
// Build: clang -fobjc-arc -Wall -O0 introspect.m -framework Foundation -o introspect
// Run:   ./introspect "$(xcode-select -p)" [--respond]
//        --respond also asks class_respondsToSelector for the named classes (may run +initialize).

#import <Foundation/Foundation.h>
#import <dlfcn.h>
#import <mach-o/dyld.h>
#import <objc/runtime.h>

static int g_missing = 0;
static BOOL g_respond = NO;

// True when cls (or a superclass) declares the method in its method list. This reads the runtime's
// tables only. It never runs +initialize or a resolver, so it cannot execute code of the framework.
// A method that a class adds lazily or answers by forwarding is not found this way.
static Method declared_method(Class cls, SEL sel, BOOL meta) {
  Class c = meta ? object_getClass(cls) : cls;
  for (; c; c = class_getSuperclass(c)) {
    unsigned int n = 0;
    Method *list = class_copyMethodList(c, &n);
    Method found = NULL;
    for (unsigned int i = 0; i < n && !found; i++) {
      if (method_getName(list[i]) == sel) found = list[i];
    }
    free(list);
    if (found) return found;
  }
  return NULL;
}

static void report(const char *kind, NSString *name, BOOL ok, NSString *detail) {
  printf("%-8s %-4s %s%s%s\n", kind, ok ? "ok" : "MISS", name.UTF8String, detail.length ? "  " : "",
         detail.UTF8String ?: "");
  if (!ok) g_missing++;
}

static NSString *encoding(Class cls, SEL sel, BOOL meta) {
  Method m = declared_method(cls, sel, meta);
  const char *t = m ? method_getTypeEncoding(m) : NULL;
  return t ? [NSString stringWithFormat:@"enc=%s", t] : @"";
}

static NSString *imageOf(Class cls) {
  const char *img = class_getImageName(cls);
  if (!img) return @"";
  NSString *s = @(img);
  NSString *last = s.lastPathComponent;
  return [NSString stringWithFormat:@"image=%@", last];
}

// Checks one class and its selectors. Entries starting with "+" are class methods, "-" instance methods.
static Class check_class(NSString *className, NSArray<NSString *> *selectors) {
  Class cls = NSClassFromString(className);
  report("class", className, cls != nil, cls ? imageOf(cls) : @"");
  if (!cls) {
    for (NSString *s in selectors) report("selector", [NSString stringWithFormat:@"%@ %@", className, s], NO, @"(class missing)");
    return nil;
  }
  for (NSString *entry in selectors) {
    BOOL meta = [entry hasPrefix:@"+"];
    SEL sel = NSSelectorFromString([entry substringFromIndex:1]);
    BOOL ok = declared_method(cls, sel, meta) != NULL;
    NSString *detail = encoding(cls, sel, meta);
    if (g_respond) {
      // Optional second check. This may run +initialize of the class, so it is off by default.
      BOOL responds = meta ? [cls respondsToSelector:sel] : class_respondsToSelector(cls, sel);
      detail = [detail stringByAppendingFormat:@"  respondsToSelector=%@", responds ? @"yes" : @"no"];
      if (responds && !ok) ok = YES;
    }
    report("selector", [NSString stringWithFormat:@"%@ %@", className, entry], ok, detail);
  }
  return cls;
}

// Classes whose instances respond to all of the selectors. Used for objects the shim only reaches
// through a getter (the io client, its ports, a port descriptor), where no class name is known.
static void find_classes_with(NSString *label, NSArray<NSString *> *selectors) {
  unsigned int n = 0;
  Class *all = objc_copyClassList(&n);
  NSMutableArray *hits = [NSMutableArray array];
  NSMutableArray *outside = [NSMutableArray array];
  for (unsigned int i = 0; i < n; i++) {
    // class_getImageName reads the image table and runs no class code.
    const char *img = class_getImageName(all[i]);
    if (!img) continue;
    NSString *image = @(img);
    BOOL every = YES;
    for (NSString *s in selectors) {
      if (!declared_method(all[i], NSSelectorFromString(s), NO)) { every = NO; break; }
    }
    if (!every) continue;
    NSString *entry = [NSString stringWithFormat:@"%s (%@)", class_getName(all[i]), image.lastPathComponent];
    if ([image containsString:@"/CoreSimulator"] || [image containsString:@"/SimulatorKit"] ||
        [image containsString:@"/AccessibilityPlatformTranslation"]) {
      [hits addObject:entry];
    } else {
      [outside addObject:entry];
    }
  }
  free(all);
  [hits sortUsingSelector:@selector(compare:)];
  [outside sortUsingSelector:@selector(compare:)];
  NSString *detail = hits.count ? [hits componentsJoinedByString:@", "] : @"no class in the three frameworks";
  if (!hits.count && outside.count) {
    detail = [detail stringByAppendingFormat:@"; in other loaded images: %@", [outside componentsJoinedByString:@", "]];
  }
  report("find", label, hits.count > 0, detail);
  // The protocols that declare the first selector, in case the class is only known at run time.
  unsigned int pn = 0;
  Protocol *__unsafe_unretained *plist = objc_copyProtocolList(&pn);
  NSMutableArray *owners = [NSMutableArray array];
  SEL first = NSSelectorFromString(selectors.firstObject);
  for (unsigned int i = 0; i < pn; i++) {
    for (int required = 0; required < 2; required++) {
      struct objc_method_description d = protocol_getMethodDescription(plist[i], first, required, YES);
      if (d.name) { [owners addObject:@(protocol_getName(plist[i]))]; break; }
    }
  }
  free(plist);
  if (owners.count) printf("%-8s      protocols declaring %s: %s\n", "find", selectors.firstObject.UTF8String,
                           [owners componentsJoinedByString:@", "].UTF8String);
}

static void check_protocol_methods(NSString *label, NSArray<NSString *> *selectors) {
  unsigned int n = 0;
  Protocol *__unsafe_unretained *list = objc_copyProtocolList(&n);
  for (NSString *sel in selectors) {
    NSMutableArray *owners = [NSMutableArray array];
    SEL s = NSSelectorFromString(sel);
    for (unsigned int i = 0; i < n; i++) {
      for (int required = 0; required < 2; required++) {
        for (int instance = 0; instance < 2; instance++) {
          struct objc_method_description d = protocol_getMethodDescription(list[i], s, required, instance);
          if (d.name) {
            [owners addObject:[NSString stringWithFormat:@"%s(%@%@)", protocol_getName(list[i]),
                               required ? @"required" : @"optional", instance ? @"" : @",class"]];
            goto next_proto;
          }
        }
      }
    next_proto:;
    }
    report("protocol", [NSString stringWithFormat:@"%@ %@", label, sel], owners.count > 0,
           owners.count ? [NSString stringWithFormat:@"declared in %@", [owners componentsJoinedByString:@", "]] : @"");
  }
  free(list);
  // Everything AXPTranslationTokenDelegateHelper declares, for comparison with what the shim implements.
  Protocol *helper = objc_getProtocol("AXPTranslationTokenDelegateHelper");
  if (helper) {
    for (int required = 0; required < 2; required++) {
      unsigned int mn = 0;
      struct objc_method_description *m = protocol_copyMethodDescriptionList(helper, required, YES, &mn);
      for (unsigned int i = 0; i < mn; i++) {
        printf("%-8s info %s %s  enc=%s\n", "protocol", required ? "required" : "optional",
               sel_getName(m[i].name), m[i].types);
      }
      free(m);
    }
  } else {
    puts("protocol info AXPTranslationTokenDelegateHelper not found");
  }
}

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    NSString *devDir = argc > 1 ? @(argv[1]) : @"/Applications/Xcode.app/Contents/Developer";
    for (int i = 2; i < argc; i++) if (strcmp(argv[i], "--respond") == 0) g_respond = YES;
    printf("process arch: %s\n",
#if defined(__arm64e__)
           "arm64e"
#elif defined(__arm64__)
           "arm64"
#elif defined(__x86_64__)
           "x86_64"
#else
           "other"
#endif
    );
    printf("developer dir: %s\n", devDir.UTF8String);

    // The same candidates and order as load_frameworks in shim.m.
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
      NSString *lastError = @"";
      for (NSString *path in paths) {
        void *h = dlopen(path.fileSystemRepresentation, RTLD_NOW | RTLD_GLOBAL);
        BOOL exists = [[NSFileManager defaultManager] fileExistsAtPath:path];
        if (h) {
          report("dlopen", path.lastPathComponent, YES, [NSString stringWithFormat:@"path=%@", path]);
          loaded = YES;
          break;
        }
        const char *e = dlerror();
        lastError = [NSString stringWithFormat:@"path=%@ fileOnDisk=%@ dlerror=%s", path, exists ? @"yes" : @"no", e ?: "none"];
      }
      if (!loaded) report("dlopen", paths.firstObject.lastPathComponent, NO, lastError);
    }

    puts("\n-- CoreSimulator classes");
    check_class(@"SimServiceContext", @[ @"+sharedServiceContextForDeveloperDir:error:", @"-defaultDeviceSetWithError:" ]);
    check_class(@"SimDeviceSet", @[ @"-devices" ]);
    check_class(@"SimDevice", @[ @"-UDID", @"-state", @"-deviceType", @"-io",
                                 @"-sendAccessibilityRequestAsync:completionQueue:completionHandler:" ]);
    check_class(@"SimDeviceType", @[ @"-mainScreenSize", @"-mainScreenScale" ]);
    // Reached only through getters, so find the class by the selectors the shim sends to it.
    find_classes_with(@"io object (-ioPorts)", @[ @"ioPorts" ]);
    find_classes_with(@"io port (-descriptor)", @[ @"descriptor" ]);
    find_classes_with(@"port descriptor (-framebufferSurface)", @[ @"framebufferSurface" ]);
    find_classes_with(@"port descriptor (-ioSurface)", @[ @"ioSurface" ]);

    puts("\n-- SimulatorKit class");
    check_class(@"SimulatorKit.SimDeviceLegacyHIDClient",
                @[ @"-initWithDevice:error:", @"-sendWithMessage:freeWhenDone:completionQueue:completion:" ]);

    puts("\n-- SimulatorKit C symbols (dlsym RTLD_DEFAULT, as the shim does)");
    NSArray *symbols = @[ @"IndigoHIDMessageForMouseNSEvent", @"IndigoHIDMessageForButton",
                          @"IndigoHIDMessageForKeyboardArbitrary" ];
    for (NSString *s in symbols) {
      void *p = dlsym(RTLD_DEFAULT, s.UTF8String);
      Dl_info info;
      NSString *detail = (p && dladdr(p, &info) && info.dli_fname) ? [NSString stringWithFormat:@"image=%@", @(info.dli_fname).lastPathComponent] : @"";
      report("symbol", s, p != NULL, detail);
    }
    puts("(extra symbols from idb's Indigo.h, not used by the shim)");
    for (NSString *s in @[ @"IndigoHIDMessageForHIDArbitrary", @"IndigoHIDMessageForScrollEvent", @"IndigoHIDMessageForMouseEvent",
                           @"IndigoHIDMessageForPressedButton", @"IndigoHIDMessageForKeyboardNSEvent" ]) {
      void *p = dlsym(RTLD_DEFAULT, s.UTF8String);
      printf("%-8s %-4s %s\n", "extra", p ? "found" : "none", s.UTF8String);
    }

    puts("\n-- AccessibilityPlatformTranslation classes");
    check_class(@"AXPTranslator", @[ @"+sharedInstance", @"-bridgeTokenDelegate", @"-setBridgeTokenDelegate:",
                                     @"-supportsDelegateTokens", @"-setSupportsDelegateTokens:",
                                     @"-accessibilityEnabled", @"-setAccessibilityEnabled:",
                                     @"-frontmostApplicationWithDisplayId:bridgeDelegateToken:",
                                     @"-translationApplicationObjectForPid:",
                                     @"-objectAtPoint:displayId:bridgeDelegateToken:",
                                     @"-macPlatformElementFromTranslation:" ]);
    check_class(@"AXPTranslationObject", @[ @"-pid", @"-bridgeDelegateToken", @"-setBridgeDelegateToken:" ]);
    check_class(@"AXPTranslatorResponse", @[ @"+emptyResponse" ]);
    check_class(@"AXPMacPlatformElement", @[ @"-translation", @"-accessibilityChildren", @"-accessibilityRole",
                                             @"-accessibilitySubrole", @"-accessibilityLabel", @"-accessibilityValue",
                                             @"-accessibilityIdentifier", @"-accessibilityPlaceholderValue",
                                             @"-accessibilityFrame", @"-isAccessibilityEnabled",
                                             @"-isAccessibilityFocused", @"-isAccessibilitySelected",
                                             @"-accessibilityAttributeNames", @"-accessibilityAttributeValue:" ]);

    puts("\n-- Bridge delegate methods the shim implements on APTranslationBridge");
    check_protocol_methods(@"delegate", @[ @"accessibilityTranslationDelegateBridgeCallbackWithToken:",
                                           @"accessibilityTranslationDelegateBridgeCallback",
                                           @"accessibilityTranslationConvertPlatformFrameToSystem:withToken:",
                                           @"accessibilityTranslationConvertPlatformFrameToSystem:withContext:postProcess:",
                                           @"accessibilityTranslationRootParentWithToken:",
                                           @"accessibilityTranslationRootParent" ]);

    printf("\nitems missing: %d\n", g_missing);
    return g_missing ? 2 : 0;
  }
}
