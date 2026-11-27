// Tapwing iOS Simulator HID and accessibility bridge (spike for issue #30).
//
// Adapted from agent-phone (https://github.com/govijr/agent-phone, MIT, Copyright (c) 2026
// Govijr, commit 4a355d4, native/shim.h) which in turn follows facebook/idb (MIT, Copyright
// (c) Meta Platforms, Inc. and affiliates). Full license texts: THIRD_PARTY_NOTICES.md.
//
// Every function takes the Xcode developer directory (the output of `xcode-select -p`) and the
// simulator UDID. It returns 0 (or a non-NULL pointer) on success. On failure it sets *err to a
// malloc'd message that the caller frees with ap_free. No Objective-C exception crosses this
// boundary. All Apple private frameworks are loaded with dlopen at run time.

#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// JSON: {"width": points, "height": points, "scale": n}. Size is the portrait size of the device
// type; it does not follow the interface orientation.
char *ap_screen(const char *dev_dir, const char *udid, char **err);

// What an accessibility snapshot reads. All fields zero means the frontmost application.
typedef struct {
  int pid;         // Non-zero: the application with this process id.
  int has_point;   // Non-zero: the element at (x, y) in screen points (hit test).
  double x, y;
  int all_attrs;   // Non-zero: add an "attrs" object with every attribute the element reports.
  int springboard; // >0: also read the application with this pid (SpringBoard) as a further root.
                   // -1: find SpringBoard's pid by hit testing the status bar, then read it.
  int max_depth;   // 0 means 64.
  int enable_ax;   // Non-zero: call -[AXPTranslator setAccessibilityEnabled:YES] before reading.
} ap_snapshot_opts;

// JSON {"roots":[tree, ...]}. Each tree node has role, subrole, roleDescription, label, title,
// value, identifier, placeholder, help, enabled, focused, selected, frame [x, y, w, h] in screen
// points, and children. Root nodes also carry "pid". timeout_s bounds each request to the
// simulator and the whole walk.
char *ap_snapshot(const char *dev_dir, const char *udid, double timeout_s,
                  const ap_snapshot_opts *opts, char **err);

// A single-finger gesture through n points (screen points). The finger goes down at the first
// point, moves through the rest with step_ms between them, and lifts at the last.
// hold_ms is how long it stays down at the first point before moving (a long press when n == 1).
int ap_touch(const char *dev_dir, const char *udid, const double *xs, const double *ys, int n,
             int hold_ms, int step_ms, char **err);

// Hardware buttons: 0 = home, 1 = lock.
int ap_button(const char *dev_dir, const char *udid, int button, char **err);

// Keyboard: n HID usages (usage page 7) with directions (1 = down, 2 = up), sent in order.
int ap_keys(const char *dev_dir, const char *udid, const uint32_t *usages,
            const uint8_t *directions, int n, char **err);

// The screen as PNG bytes (quality <= 0) or JPEG bytes (quality 0-1). *len is set. Free with
// ap_free.
char *ap_frame(const char *dev_dir, const char *udid, double quality, size_t *len, char **err);

void ap_free(char *s);

#ifdef __cplusplus
}
#endif
