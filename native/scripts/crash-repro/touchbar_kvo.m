// Reproduces the SIGABRT in -[_NSTouchBarFinderObservation invalidate].
// It replays gpui's window teardown against AppKit's Touch Bar finder: accesskit's
// SubclassingAdapter swaps the content view's class while the window lives, the finder
// KVO-observes the key window's responders, and restoring the class while observed makes
// the finder's later removeObserver: raise an uncaught NSRangeException.
//
// usage: touchbar_kvo old|fixed [iterations]
//   old:   adapter dropped (class restored) before the window leaves the responder chain -> aborts
//   fixed: makeFirstResponder:nil + close, restore the class once no longer observed   -> survives
// Needs a Mac with a Touch Bar (the finder only runs there) and briefly takes focus.
#import <Cocoa/Cocoa.h>
#import <objc/runtime.h>
@interface FirstResponderView : NSView
@end
@implementation FirstResponderView
- (BOOL)acceptsFirstResponder { return YES; }
@end
static BOOL fixed;
static int remaining;
static void cycle(void) {
  if (remaining-- == 0) {
    fprintf(stderr, "survived (%s)\n", fixed ? "fixed" : "old");
    [NSApp terminate:nil];
    return;
  }
  NSWindow *w = [[NSWindow alloc] initWithContentRect:NSMakeRect(100, 100, 300, 200)
                                            styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable
                                              backing:NSBackingStoreBuffered
                                                defer:NO];
  w.releasedWhenClosed = NO;
  FirstResponderView *v = [[FirstResponderView alloc] initWithFrame:w.contentView.bounds];
  [w.contentView addSubview:v];
  [w makeFirstResponder:v];
  Class prev = object_getClass(w.contentView);
  static Class sub;
  if (!sub) {
    sub = objc_allocateClassPair(prev, "AccessKitSubclassOfNSView", 0);
    objc_registerClassPair(sub);
  }
  object_setClass(w.contentView, sub);  // SubclassingAdapter::for_window
  [w makeKeyAndOrderFront:nil];
  [NSApp activate];
  [[NSRunningApplication currentApplication] activateWithOptions:NSApplicationActivateAllWindows];
  __block int polls = 0;
  __block void (^teardown)(void);
  void (^wait_observed)(void) = ^{
    // Wait (up to 10 s) until the Touch Bar finder observes the key window's content view.
    if (![w.contentView observationInfo] && polls++ < 40) {
      [w makeKeyAndOrderFront:nil];
      [w makeFirstResponder:nil];
      [w makeFirstResponder:v];
      dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 250 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{ teardown(); });
      return;
    }
    fprintf(stderr, "observed by AppKit before teardown: %s (app active: %s)\n", [w.contentView observationInfo] ? "yes" : "no", NSApp.active ? "yes" : "no");
    if (!fixed) {
      object_setClass(w.contentView, prev);  // SubclassingAdapter::drop, then close
      [w close];
      dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 300 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{ cycle(); });
    } else {
      [w makeFirstResponder:nil];
      [w close];
      dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 500 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{
        fprintf(stderr, "observed after close: %s\n", [w.contentView observationInfo] ? "yes" : "no");
        object_setClass(w.contentView, prev);
        cycle();
      });
    }
  };
  teardown = wait_observed;
  dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 500 * NSEC_PER_MSEC), dispatch_get_main_queue(), ^{ teardown(); });
}
int main(int argc, char **argv) {
  @autoreleasepool {
    fixed = argc > 1 && strcmp(argv[1], "fixed") == 0;
    remaining = argc > 2 ? atoi(argv[2]) : 3;
    [NSApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
    dispatch_async(dispatch_get_main_queue(), ^{ cycle(); });
    [NSApp run];
  }
}
