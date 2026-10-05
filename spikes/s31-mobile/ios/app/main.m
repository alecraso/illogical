// S31 probe: an iOS app that reports what it may do toward running
// terminals, and (with the `bg` argument) logs a heartbeat to show how long
// it keeps running once backgrounded.
#import <UIKit/UIKit.h>

void s31_probe(const char *bundle, const char *docs, void (*emit)(const char *));
void s31_wasm_bench(const char *path, void (*emit)(const char *));
void s31_wasm_repl(const char *path, void (*emit)(const char *));
char *s31_rt_selftest(void);
void s31_rt_heartbeat(void (*log)(const char *));
void s31_free(char *);
void s31_jit_exec(void (*emit)(const char *));

static FILE *g_log;
static UITextView *g_view;
static NSMutableString *g_text;

static void emit(const char *line) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    double t = ts.tv_sec + ts.tv_nsec / 1e9;
    fprintf(g_log, "%.3f %s\n", t, line);
    fflush(g_log);
    fprintf(stderr, "S31 %s\n", line);
    NSString *s = [NSString stringWithUTF8String:line];
    dispatch_async(dispatch_get_main_queue(), ^{
      [g_text appendFormat:@"%@\n", s];
      g_view.text = g_text;
    });
}

@interface AppDelegate : UIResponder <UIApplicationDelegate>
@property(strong, nonatomic) UIWindow *window;
@property(nonatomic) UIBackgroundTaskIdentifier task;
@end

@implementation AppDelegate
- (BOOL)application:(UIApplication *)app didFinishLaunchingWithOptions:(NSDictionary *)opts {
    NSString *docs = NSSearchPathForDirectoriesInDomains(NSDocumentDirectory, NSUserDomainMask, YES)[0];
    NSArray *args = NSProcessInfo.processInfo.arguments;
    BOOL bg = [args containsObject:@"bg"];
    NSString *logName = bg ? @"bg.txt" : @"results.txt";
    g_log = fopen([docs stringByAppendingPathComponent:logName].UTF8String, "w");
    g_text = [NSMutableString new];

    self.window = [[UIWindow alloc] initWithFrame:UIScreen.mainScreen.bounds];
    UIViewController *vc = [UIViewController new];
    g_view = [[UITextView alloc] initWithFrame:vc.view.bounds];
    g_view.editable = NO;
    g_view.font = [UIFont monospacedSystemFontOfSize:10 weight:UIFontWeightRegular];
    g_view.autoresizingMask = UIViewAutoresizingFlexibleWidth | UIViewAutoresizingFlexibleHeight;
    [vc.view addSubview:g_view];
    self.window.rootViewController = vc;
    [self.window makeKeyAndVisible];

    NSString *bundle = NSBundle.mainBundle.bundlePath;
    if (bg) {
        emit([NSString stringWithFormat:@"bg mode, bgtask=%d", [args containsObject:@"bgtask"]].UTF8String);
        [NSThread detachNewThreadWithBlock:^{
          for (unsigned long n = 0;; n++) {
              emit([NSString stringWithFormat:@"tick %lu", n].UTF8String);
              sleep(1);
          }
        }];
        s31_rt_heartbeat(emit);
        return YES;
    }
    [NSThread detachNewThreadWithBlock:^{
      emit([NSString stringWithFormat:@"iOS %@ on %@", UIDevice.currentDevice.systemVersion, UIDevice.currentDevice.model].UTF8String);
      s31_probe(bundle.UTF8String, docs.UTF8String, emit);
      emit("--- wasm3 (interpreter)");
      s31_wasm_bench([bundle stringByAppendingPathComponent:@"bench.wasm"].UTF8String, emit);
      s31_wasm_repl([bundle stringByAppendingPathComponent:@"repl.wasm"].UTF8String, emit);
      emit("--- rust: tokio, axum, reqwest, tungstenite, lib-vt");
      char *r = s31_rt_selftest();
      for (NSString *l in [[NSString stringWithUTF8String:r] componentsSeparatedByString:@"\n"]) emit(l.UTF8String);
      s31_free(r);
      emit("--- done");
      s31_jit_exec(emit);
    }];
    return YES;
}

- (void)applicationDidEnterBackground:(UIApplication *)app {
    emit([NSString stringWithFormat:@"entered background, backgroundTimeRemaining %.1f", app.backgroundTimeRemaining].UTF8String);
    if ([NSProcessInfo.processInfo.arguments containsObject:@"bgtask"]) {
        self.task = [app beginBackgroundTaskWithName:@"s31" expirationHandler:^{
          emit("background task expired");
          [app endBackgroundTask:self.task];
        }];
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC), dispatch_get_main_queue(), ^{
          emit([NSString stringWithFormat:@"with a background task, remaining %.1f", app.backgroundTimeRemaining].UTF8String);
        });
    }
}
- (void)applicationWillEnterForeground:(UIApplication *)app {
    emit("entering foreground");
}
- (void)applicationWillTerminate:(UIApplication *)app {
    emit("will terminate");
}
@end

int main(int argc, char *argv[]) {
    @autoreleasepool {
        return UIApplicationMain(argc, argv, nil, NSStringFromClass([AppDelegate class]));
    }
}
