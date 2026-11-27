// Tap delivery test for issue #30, phase 3. One process, one HID client, like Tapwing.
// Taps a Flutter Switch N times and reads the switch colour from the framebuffer after each tap.
// usage: tapcheck <udid> <n> <hold_ms> <x_pt> <y_pt> <px_x> <px_y> [settle_ms] [late_ms]
// Output: one CSV line per tap: i,before,after,late,result,tap_ms   result = ok | slow | lost | unreadable
// before/after/late are 1 (green track = on), 0 (off) or -1 (unreadable).
#import <Foundation/Foundation.h>
#import <ImageIO/ImageIO.h>
#import <CoreGraphics/CoreGraphics.h>
#import "shim.h"

static double now_ms(void) { return [NSDate timeIntervalSinceReferenceDate] * 1000.0; }

static int read_state(const char *dev, const char *udid, int px, int py) {
  char *err = NULL; size_t len = 0;
  char *png = ap_frame(dev, udid, 0, &len, &err);
  if (!png) { if (err) ap_free(err); return -1; }
  NSData *d = [NSData dataWithBytes:png length:len]; ap_free(png);
  CGImageSourceRef src = CGImageSourceCreateWithData((__bridge CFDataRef)d, NULL);
  CGImageRef img = src ? CGImageSourceCreateImageAtIndex(src, 0, NULL) : NULL;
  if (src) CFRelease(src);
  if (!img) return -1;
  uint8_t px4[4] = {0};
  CGColorSpaceRef cs = CGColorSpaceCreateDeviceRGB();
  CGContextRef c = CGBitmapContextCreate(px4, 1, 1, 8, 4, cs, kCGImageAlphaPremultipliedLast);
  CGContextDrawImage(c, CGRectMake(-px, -((int)CGImageGetHeight(img) - 1 - py), CGImageGetWidth(img), CGImageGetHeight(img)), img);
  CGContextRelease(c); CGColorSpaceRelease(cs); CGImageRelease(img);
  int r = px4[0], g = px4[1], b = px4[2];
  if (g > r + 40 && g > b) return 1;           // green track: on
  if (abs(r - g) < 30 && r > 90 && r < 150) return 0;  // grey thumb: off
  return -1;
}

int main(int argc, char **argv) {
  @autoreleasepool {
    if (argc < 8) { fprintf(stderr, "usage\n"); return 2; }
    const char *udid = argv[1];
    int n = atoi(argv[2]), hold = atoi(argv[3]);
    double x = atof(argv[4]), y = atof(argv[5]);
    int px = atoi(argv[6]), py = atoi(argv[7]);
    int settle = argc > 8 ? atoi(argv[8]) : 700, late = argc > 9 ? atoi(argv[9]) : 1500;
    const char *dev = getenv("DEVELOPER_DIR");
    static char buf[512];
    if (!dev) { FILE *f = popen("xcode-select -p", "r"); if (!f || !fgets(buf, sizeof buf, f)) return 3; pclose(f); buf[strcspn(buf, "\n")] = 0; dev = buf; }
    char *err = NULL;
    int before = read_state(dev, udid, px, py);
    for (int i = 0; i < n; i++) {
      double t = now_ms();
      double xs[1] = {x}, ys[1] = {y};
      int rc = ap_touch(dev, udid, xs, ys, 1, hold, 0, &err);
      double tap_ms = now_ms() - t;
      if (rc) { printf("%d,%d,-1,-1,error:%s,%.0f\n", i, before, err ?: "?", tap_ms); fflush(stdout); if (err) { ap_free(err); err = NULL; } usleep(500000); before = read_state(dev, udid, px, py); continue; }
      usleep(settle * 1000);
      int after = read_state(dev, udid, px, py);
      int lateS = after;
      const char *res;
      if (before < 0 || after < 0) {
        usleep(late * 1000); lateS = read_state(dev, udid, px, py);
        res = "unreadable";
        if (before >= 0 && lateS >= 0) res = lateS != before ? "slow" : "lost";
      } else if (after != before) res = "ok";
      else { usleep(late * 1000); lateS = read_state(dev, udid, px, py); res = lateS != before ? "slow" : "lost"; }
      printf("%d,%d,%d,%d,%s,%.0f\n", i, before, after, lateS, res, tap_ms); fflush(stdout);
      before = lateS >= 0 ? lateS : after;
      usleep(300000);
    }
  }
  return 0;
}
