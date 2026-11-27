// hid-probe: command line driver for crates/tapwing/native/ios_hid/shim.m (spike for issue #30).
//
//   hid-probe screen   <udid>
//   hid-probe snapshot <udid> [--repeat n] [--pid p] [--point x,y] [--all-attrs] [--springboard] [--enable-ax] [--out file]
//   hid-probe tap      <udid> <x> <y> [--hold ms] [--repeat n]
//   hid-probe swipe    <udid> <x1> <y1> <x2> <y2> [ms]
//   hid-probe button   <udid> home|lock
//   hid-probe frame    <udid> <out.png> [--repeat n]
//   hid-probe type     <udid> <ascii text>
//
// Results go to stdout (JSON for snapshot, nothing for input commands). Durations go to stderr as
// "<command> ... <ms> ms". With --repeat the first call is reported separately from later ones.

#import <Foundation/Foundation.h>
#import <mach/mach_time.h>
#import <sys/sysctl.h>
#import "shim.h"

static double now_ms(void) {
  static mach_timebase_info_data_t tb;
  if (!tb.denom) {
    mach_timebase_info(&tb);
  }
  return (double)mach_absolute_time() * tb.numer / tb.denom / 1e6;
}

static double since_process_start_ms(void) {
  struct kinfo_proc info;
  size_t size = sizeof info;
  int mib[4] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()};
  if (sysctl(mib, 4, &info, &size, NULL, 0) != 0) {
    return -1;
  }
  struct timeval start = info.kp_proc.p_un.__p_starttime, nowtv;
  gettimeofday(&nowtv, NULL);
  return (nowtv.tv_sec - start.tv_sec) * 1000.0 + (nowtv.tv_usec - start.tv_usec) / 1000.0;
}

static char *developer_dir(void) {
  const char *env = getenv("DEVELOPER_DIR");
  if (env && *env) {
    return strdup(env);
  }
  FILE *p = popen("xcode-select -p", "r");
  char buf[1024] = {0};
  if (!p || !fgets(buf, sizeof buf, p)) {
    fprintf(stderr, "error: xcode-select -p failed\n");
    exit(2);
  }
  pclose(p);
  buf[strcspn(buf, "\n")] = 0;
  return strdup(buf);
}

static int fail(char *err) {
  fprintf(stderr, "error: %s\n", err ?: "unknown");
  ap_free(err);
  return 1;
}

static void usage(void) {
  fprintf(stderr,
          "usage: hid-probe <screen|snapshot|tap|swipe|button|frame|type> <udid> [args]\n"
          "  snapshot [--repeat n] [--pid p] [--point x,y] [--all-attrs] [--springboard] [--enable-ax] [--out file] [--timeout s]\n"
          "  tap x y [--hold ms]    swipe x1 y1 x2 y2 [ms]    button home|lock\n"
          "  frame out.png [--repeat n]    type <ascii text>\n");
}

static void print_repeat(const char *name, int n, double *ms) {
  fprintf(stderr, "%s call 1 (first): %.1f ms\n", name, ms[0]);
  for (int i = 1; i < n; i++) {
    fprintf(stderr, "%s call %d: %.1f ms\n", name, i + 1, ms[i]);
  }
}

// US keyboard layout: ASCII character to HID usage (page 7) and whether shift is needed.
static int hid_usage(char c, int *shift) {
  *shift = 0;
  if (c >= 'a' && c <= 'z') return 0x04 + (c - 'a');
  if (c >= 'A' && c <= 'Z') { *shift = 1; return 0x04 + (c - 'A'); }
  if (c >= '1' && c <= '9') return 0x1e + (c - '1');
  if (c == '0') return 0x27;
  if (c == ' ') return 0x2c;
  if (c == '\n') return 0x28;
  if (c == '-') return 0x2d;
  if (c == '.') return 0x37;
  if (c == ',') return 0x36;
  if (c == '@') { *shift = 1; return 0x1f; }
  return -1;
}

int main(int argc, char **argv) {
  @autoreleasepool {
    if (argc < 3) {
      usage();
      return 2;
    }
    const char *cmd = argv[1], *udid = argv[2];
    char *dev = developer_dir();
    char *err = NULL;
    int repeat = 1, hold = 50, pid = 0, all_attrs = 0, sb = 0, has_point = 0, enable_ax = 0;
    double px = 0, py = 0, timeout = 5;
    const char *out_path = NULL;
    NSMutableArray<NSString *> *pos = [NSMutableArray array];
    for (int i = 3; i < argc; i++) {
      if (!strcmp(argv[i], "--repeat") && i + 1 < argc) repeat = atoi(argv[++i]);
      else if (!strcmp(argv[i], "--hold") && i + 1 < argc) hold = atoi(argv[++i]);
      else if (!strcmp(argv[i], "--pid") && i + 1 < argc) pid = atoi(argv[++i]);
      else if (!strcmp(argv[i], "--point") && i + 1 < argc) has_point = sscanf(argv[++i], "%lf,%lf", &px, &py) == 2;
      else if (!strcmp(argv[i], "--out") && i + 1 < argc) out_path = argv[++i];
      else if (!strcmp(argv[i], "--timeout") && i + 1 < argc) timeout = atof(argv[++i]);
      else if (!strcmp(argv[i], "--all-attrs")) all_attrs = 1;
      else if (!strcmp(argv[i], "--enable-ax")) enable_ax = 1;
      else if (!strcmp(argv[i], "--springboard")) sb = -1;
      else [pos addObject:@(argv[i])];
    }
    if (repeat < 1) repeat = 1;
    double *ms = calloc(repeat, sizeof(double));

    if (!strcmp(cmd, "screen")) {
      double t = now_ms();
      char *json = ap_screen(dev, udid, &err);
      if (!json) return fail(err);
      printf("%s\n", json);
      fprintf(stderr, "screen: %.1f ms (process start to result: %.1f ms)\n", now_ms() - t, since_process_start_ms());
      ap_free(json);
    } else if (!strcmp(cmd, "snapshot")) {
      ap_snapshot_opts o = {.pid = pid, .has_point = has_point, .x = px, .y = py, .all_attrs = all_attrs, .springboard = sb, .enable_ax = enable_ax};
      char *last = NULL;
      for (int i = 0; i < repeat; i++) {
        double t = now_ms();
        char *json = ap_snapshot(dev, udid, timeout, &o, &err);
        ms[i] = now_ms() - t;
        if (!json) return fail(err);
        if (i == 0) fprintf(stderr, "snapshot: first call, process start to JSON: %.1f ms\n", since_process_start_ms());
        if (last) ap_free(last);
        last = json;
      }
      if (out_path) {
        FILE *f = fopen(out_path, "w");
        fputs(last, f);
        fclose(f);
      } else {
        printf("%s\n", last);
      }
      print_repeat("snapshot", repeat, ms);
      fprintf(stderr, "snapshot bytes: %zu\n", strlen(last));
      ap_free(last);
    } else if (!strcmp(cmd, "tap") && pos.count >= 2) {
      double x = pos[0].doubleValue, y = pos[1].doubleValue;
      for (int i = 0; i < repeat; i++) {
        double t = now_ms();
        if (ap_touch(dev, udid, &x, &y, 1, hold, 0, &err)) return fail(err);
        ms[i] = now_ms() - t;
        if (i == 0) fprintf(stderr, "tap: first call, process start to done: %.1f ms\n", since_process_start_ms());
      }
      print_repeat("tap", repeat, ms);
    } else if (!strcmp(cmd, "swipe") && pos.count >= 4) {
      double xs[2] = {pos[0].doubleValue, pos[2].doubleValue}, ys[2] = {pos[1].doubleValue, pos[3].doubleValue};
      int dur = pos.count > 4 ? pos[4].intValue : 300;
      // Intermediate points every ~16 ms so the scroll view sees a drag.
      int steps = dur / 16 > 2 ? dur / 16 : 2;
      double sx[steps + 1], sy[steps + 1];
      for (int i = 0; i <= steps; i++) {
        sx[i] = xs[0] + (xs[1] - xs[0]) * i / steps;
        sy[i] = ys[0] + (ys[1] - ys[0]) * i / steps;
      }
      double t = now_ms();
      if (ap_touch(dev, udid, sx, sy, steps + 1, 30, dur / steps, &err)) return fail(err);
      fprintf(stderr, "swipe (%g,%g)->(%g,%g) %d ms: %.1f ms\n", xs[0], ys[0], xs[1], ys[1], dur, now_ms() - t);
    } else if (!strcmp(cmd, "button") && pos.count >= 1) {
      int b = [pos[0] isEqualToString:@"lock"] ? 1 : 0;
      if (![pos[0] isEqualToString:@"lock"] && ![pos[0] isEqualToString:@"home"]) {
        usage();
        return 2;
      }
      double t = now_ms();
      if (ap_button(dev, udid, b, &err)) return fail(err);
      fprintf(stderr, "button %s: %.1f ms\n", pos[0].UTF8String, now_ms() - t);
    } else if (!strcmp(cmd, "frame") && pos.count >= 1) {
      char *last = NULL;
      size_t len = 0;
      for (int i = 0; i < repeat; i++) {
        double t = now_ms();
        size_t l = 0;
        char *png = ap_frame(dev, udid, 0, &l, &err);
        ms[i] = now_ms() - t;
        if (!png) return fail(err);
        if (i == 0) fprintf(stderr, "frame: first call, process start to PNG: %.1f ms\n", since_process_start_ms());
        if (last) ap_free(last);
        last = png;
        len = l;
      }
      NSData *data = [NSData dataWithBytes:last length:len];
      [data writeToFile:pos[0] atomically:YES];
      print_repeat("frame", repeat, ms);
      fprintf(stderr, "frame bytes: %zu\n", len);
      ap_free(last);
    } else if (!strcmp(cmd, "type") && pos.count >= 1) {
      const char *text = pos[0].UTF8String;
      uint32_t usages[512];
      uint8_t dirs[512];
      int n = 0;
      for (const char *c = text; *c && n < 500; c++) {
        int shift;
        int u = hid_usage(*c, &shift);
        if (u < 0) {
          fprintf(stderr, "error: no HID keycode for byte 0x%02x (only US ASCII letters, digits, space, - . , @)\n", (unsigned char)*c);
          return 1;
        }
        if (shift) { usages[n] = 0xe1; dirs[n++] = 1; }
        usages[n] = u; dirs[n++] = 1;
        usages[n] = u; dirs[n++] = 2;
        if (shift) { usages[n] = 0xe1; dirs[n++] = 2; }
      }
      double t = now_ms();
      if (ap_keys(dev, udid, usages, dirs, n, &err)) return fail(err);
      fprintf(stderr, "type %zu chars: %.1f ms\n", strlen(text), now_ms() - t);
    } else {
      usage();
      return 2;
    }
    return 0;
  }
}
