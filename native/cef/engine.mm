// Optional CEF backend. All browser access is on CEF's UI thread.
#include "bridge.h"
#include "include/cef_app.h"
#include "include/cef_application_mac.h"
#include "include/cef_browser.h"
#include "include/cef_client.h"
#include "include/cef_command_line.h"
#include "include/cef_parser.h"
#include "include/cef_render_handler.h"
#include "include/cef_task.h"
#include "include/wrapper/cef_helpers.h"
#include "include/wrapper/cef_library_loader.h"
#import <AppKit/AppKit.h>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <crt_externs.h>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <time.h>
namespace {
LuchsCefCallbacks callbacks;
void *context;
LuchsCefConfig config;
std::string page_url;
CefRefPtr<CefBrowser> browser;
uint32_t width, height;
double scale = 1.;
bool closing = false;
std::mutex dispatch_mutex;
bool available = false;
std::map<uint64_t, CefKeyEvent> keys;
std::map<int, CefMouseEvent> buttons;
std::map<uint64_t, CefKeyEvent> deferred;
uint64_t host_time() {
  struct timespec ts;
  clock_gettime(CLOCK_UPTIME_RAW, &ts);
  return uint64_t(ts.tv_sec) * 1000000000ull + ts.tv_nsec;
}
NSString *ns(const CefString &s) {
  return [NSString stringWithUTF8String:s.ToString().c_str()];
}
void state(NSString *domain, NSDictionary *body) {
  NSDictionary *value = @{@"domain" : domain, @"body" : body};
  NSData *data = [NSJSONSerialization dataWithJSONObject:value
                                                 options:0
                                                   error:nil];
  std::string json((const char *)data.bytes, data.length);
  callbacks.state(context, json.c_str());
}
void nav(bool loading) {
  if (!browser)
    return;
  state(
      @"navigation", @{
        @"url" : ns(browser->GetMainFrame()->GetURL()),
        @"title" : @"",
        @"can_go_back" : @(browser->CanGoBack()),
        @"can_go_forward" : @(browser->CanGoForward()),
        @"loading" : @(loading),
        @"capabilities" : @{}
      });
}
class Task : public CefTask {
public:
  explicit Task(std::function<void()> f) : f_(std::move(f)) {}
  void Execute() override { f_(); }

private:
  std::function<void()> f_;
  IMPLEMENT_REFCOUNTING(Task);
};
void post(std::function<void()> f) {
  CefPostTask(TID_UI, new Task(std::move(f)));
}
void cleanup(bool all) {
  if (!browser)
    return;
  auto host = browser->GetHost();
  for (auto [b, event] : buttons)
    host->SendMouseClickEvent(event,
                              b == 1   ? MBT_LEFT
                              : b == 2 ? MBT_RIGHT
                                       : MBT_MIDDLE,
                              true, 1);
  buttons.clear();
  if (all) {
    for (auto [press, event] : keys) {
      event.type = KEYEVENT_KEYUP;
      host->SendKeyEvent(event);
    }
    keys.clear();
    deferred.clear();
  }
}
int flags(NSInteger m) {
  return ((m & 1) ? EVENTFLAG_SHIFT_DOWN : 0) |
         ((m & 2) ? EVENTFLAG_CONTROL_DOWN : 0) |
         ((m & 4) ? EVENTFLAG_ALT_DOWN : 0) |
         ((m & 8) ? EVENTFLAG_COMMAND_DOWN : 0) |
         ((m & 16) ? EVENTFLAG_CAPS_LOCK_ON : 0);
}
int windows_key(int native) {
  static const int letters[][2] = {
      {0, 'A'},  {1, 'S'},  {2, 'D'},  {3, 'F'},  {4, 'H'},  {5, 'G'},
      {6, 'Z'},  {7, 'X'},  {8, 'C'},  {9, 'V'},  {11, 'B'}, {12, 'Q'},
      {13, 'W'}, {14, 'E'}, {15, 'R'}, {16, 'Y'}, {17, 'T'}, {31, 'O'},
      {32, 'U'}, {34, 'I'}, {35, 'P'}, {37, 'L'}, {38, 'J'}, {40, 'K'},
      {45, 'N'}, {46, 'M'}, {18, '1'}, {19, '2'}, {20, '3'}, {21, '4'},
      {23, '5'}, {22, '6'}, {26, '7'}, {28, '8'}, {25, '9'}, {29, '0'}};
  for (auto pair : letters)
    if (pair[0] == native)
      return pair[1];
  switch (native) {
  case 36:
    return 13;
  case 48:
    return 9;
  case 49:
    return 32;
  case 51:
    return 8;
  case 53:
    return 27;
  case 117:
    return 46;
  case 123:
    return 37;
  case 124:
    return 39;
  case 125:
    return 40;
  case 126:
    return 38;
  case 115:
    return 36;
  case 119:
    return 35;
  case 116:
    return 33;
  case 121:
    return 34;
  case 55:
  case 54:
    return 91;
  case 56:
  case 60:
    return 16;
  case 59:
  case 62:
    return 17;
  case 58:
  case 61:
    return 18;
  default:
    return 0;
  }
}
int execute(NSDictionary *c) {
  if (!browser || closing)
    return 2;
  NSString *kind = c[@"type"];
  auto host = browser->GetHost();
  if ([kind isEqual:@"cleanup"]) {
    cleanup([c[@"scope"] isEqual:@"all"]);
    return 0;
  }
  if ([kind isEqual:@"resize"]) {
    double next_scale = [c[@"scale"] doubleValue];
    uint32_t w = [c[@"width"] unsignedIntValue],
             h = [c[@"height"] unsignedIntValue];
    if (!w || !h || !isfinite(next_scale) || next_scale <= 0 ||
        double(w) * h * next_scale * next_scale * 4 > 64 * 1024 * 1024)
      return 2;
    cleanup(false);
    width = w;
    height = h;
    scale = next_scale;
    host->NotifyScreenInfoChanged();
    host->WasResized();
    return 0;
  }
  if ([kind isEqual:@"presentation"]) {
    host->WasHidden(![c[@"visible"] boolValue]);
    return 0;
  }
  if ([kind isEqual:@"focus"]) {
    bool focused = [c[@"focused"] boolValue];
    if (!focused)
      cleanup(true);
    host->SetFocus(focused);
    return 0;
  }
  if ([kind isEqual:@"navigation.load"]) {
    NSString *url = c[@"url"];
    if (!callbacks.allowed(context, url.UTF8String))
      return 2;
    browser->GetMainFrame()->LoadURL(url.UTF8String);
    return 0;
  }
  if ([kind isEqual:@"navigation.back"]) {
    browser->GoBack();
    return 0;
  }
  if ([kind isEqual:@"navigation.forward"]) {
    browser->GoForward();
    return 0;
  }
  if ([kind isEqual:@"navigation.reload"] || [kind isEqual:@"reload"]) {
    browser->ReloadIgnoreCache();
    return 0;
  }
  if ([kind isEqual:@"navigation.stop"]) {
    browser->StopLoad();
    return 0;
  }
  if ([kind isEqual:@"navigation.rejected"]) {
    fprintf(stderr, "luchs cef: rejected navigation\n");
    return 0;
  }
  CefMouseEvent mouse;
  mouse.x = [c[@"x"] intValue];
  mouse.y = [c[@"y"] intValue];
  mouse.modifiers = flags([c[@"modifiers"] integerValue]);
  if ([kind isEqual:@"mouse_move"]) {
    host->SendMouseMoveEvent(mouse, false);
    return 0;
  }
  if ([kind isEqual:@"mouse_down"] || [kind isEqual:@"mouse_up"]) {
    int b = [c[@"button"] intValue];
    if (b < 1 || b > 3)
      return 1;
    bool up = [kind isEqual:@"mouse_up"];
    host->SendMouseClickEvent(mouse,
                              b == 1   ? MBT_LEFT
                              : b == 2 ? MBT_RIGHT
                                       : MBT_MIDDLE,
                              up, 1);
    if (up)
      buttons.erase(b);
    else
      buttons[b] = mouse;
    return 0;
  }
  if ([kind isEqual:@"scroll"]) {
    host->SendMouseWheelEvent(mouse, -[c[@"point_dx"] intValue],
                              -[c[@"point_dy"] intValue]);
    return 0;
  }
  if ([kind isEqual:@"text"]) {
    NSString *text = c[@"text"];
    if (!text)
      return 1;
    bool cooperative = [c[@"cooperative"] boolValue];
    if (cooperative && !deferred.empty()) {
      auto [press, event] = *deferred.begin();
      deferred.erase(press);
      host->SendKeyEvent(event);
    }
    for (NSUInteger i = 0; i < text.length; i++) {
      CefKeyEvent e;
      e.type = KEYEVENT_CHAR;
      e.character = [text characterAtIndex:i];
      e.unmodified_character = e.character;
      host->SendKeyEvent(e);
    }
    return 0;
  }
  if ([kind isEqual:@"key_down"] || [kind isEqual:@"key_up"]) {
    uint64_t press = [c[@"press"] unsignedLongLongValue];
    bool up = [kind isEqual:@"key_up"];
    CefKeyEvent e;
    e.native_key_code = [c[@"key_code"] intValue];
    e.windows_key_code = windows_key(e.native_key_code);
    e.modifiers = flags([c[@"modifiers"] integerValue]);
    e.type = up ? KEYEVENT_KEYUP : KEYEVENT_RAWKEYDOWN;
    NSString *logical = c[@"logical"];
    if ([logical isKindOfClass:NSString.class] && logical.length) {
      e.character = [logical characterAtIndex:0];
      e.unmodified_character = e.character;
    }
    if (up) {
      auto held = keys.find(press);
      if (held == keys.end())
        return 0;
      e = held->second;
      e.type = KEYEVENT_KEYUP;
      deferred.erase(press);
      keys.erase(held);
      host->SendKeyEvent(e);
      return 0;
    }
    if (!e.windows_key_code && e.native_key_code != 65535)
      return 1;
    keys[press] = e;
    bool printable =
        (e.windows_key_code >= 32 && e.windows_key_code <= 126) &&
        !(e.modifiers & (EVENTFLAG_CONTROL_DOWN | EVENTFLAG_COMMAND_DOWN));
    if ([c[@"cooperative"] boolValue] && printable)
      deferred[press] = e;
    else
      host->SendKeyEvent(e);
    return 0;
  }
  return 1;
}
class Client : public CefClient,
               public CefRenderHandler,
               public CefLifeSpanHandler,
               public CefLoadHandler,
               public CefDisplayHandler,
               public CefRequestHandler,
               public CefResourceRequestHandler {
public:
  CefRefPtr<CefRenderHandler> GetRenderHandler() override { return this; }
  CefRefPtr<CefLifeSpanHandler> GetLifeSpanHandler() override { return this; }
  CefRefPtr<CefLoadHandler> GetLoadHandler() override { return this; }
  CefRefPtr<CefDisplayHandler> GetDisplayHandler() override { return this; }
  CefRefPtr<CefRequestHandler> GetRequestHandler() override { return this; }
  void GetViewRect(CefRefPtr<CefBrowser>, CefRect &rect) override {
    rect = CefRect(0, 0, width, height);
  }
  bool GetScreenInfo(CefRefPtr<CefBrowser>, CefScreenInfo &info) override {
    info.device_scale_factor = scale;
    info.rect = CefRect(0, 0, width, height);
    info.available_rect = info.rect;
    return true;
  }
  void OnPaint(CefRefPtr<CefBrowser>, PaintElementType type, const RectList &,
               const void *buffer, int w, int h) override {
    if (type == PET_VIEW)
      callbacks.pixels(context, (const uint8_t *)buffer, w, h, host_time());
  }
  void OnAcceleratedPaint(CefRefPtr<CefBrowser>, PaintElementType type,
                          const RectList &,
                          const CefAcceleratedPaintInfo &info) override {
    if (type == PET_VIEW)
      callbacks.surface(context, info.shared_texture_io_surface,
                        info.extra.coded_size.width,
                        info.extra.coded_size.height, host_time());
  }
  void OnAfterCreated(CefRefPtr<CefBrowser> b) override { browser = b; }
  void OnBeforeClose(CefRefPtr<CefBrowser>) override {
    browser = nullptr;
    CefQuitMessageLoop();
  }
  void OnLoadingStateChange(CefRefPtr<CefBrowser>, bool loading, bool,
                            bool) override {
    nav(loading);
    if (!loading)
      state(
          @"window", @{
            @"title" : @"CEF page",
            @"requested_size" : [NSNull null],
            @"ready" : @YES
          });
  }
  void OnTitleChange(CefRefPtr<CefBrowser>, const CefString &title) override {
    state(
        @"window", @{
          @"title" : ns(title),
          @"requested_size" : [NSNull null],
          @"ready" : @YES
        });
  }
  void OnAddressChange(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                       const CefString &) override {
    if (frame->IsMain())
      nav(true);
  }
  CefRefPtr<CefResourceRequestHandler>
  GetResourceRequestHandler(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                            CefRefPtr<CefRequest>, bool, bool,
                            const CefString &, bool &) override {
    return this;
  }
  ReturnValue OnBeforeResourceLoad(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                                   CefRefPtr<CefRequest> request,
                                   CefRefPtr<CefCallback>) override {
    auto url = request->GetURL().ToString();
    return url.starts_with("file:") && !callbacks.allowed(context, url.c_str())
               ? RV_CANCEL
               : RV_CONTINUE;
  }
  void OnResourceRedirect(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>,
                          CefRefPtr<CefRequest>, CefRefPtr<CefResponse>,
                          CefString &new_url) override {
    auto url = new_url.ToString();
    if (url.starts_with("file:") && !callbacks.allowed(context, url.c_str()))
      new_url = "about:blank";
  }
  bool OnBeforePopup(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame>, int,
                     const CefString &, const CefString &,
                     WindowOpenDisposition, bool, const CefPopupFeatures &,
                     CefWindowInfo &, CefRefPtr<CefClient> &,
                     CefBrowserSettings &, CefRefPtr<CefDictionaryValue> &,
                     bool *) override {
    return true;
  }
  bool OnBeforeBrowse(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                      CefRefPtr<CefRequest> request, bool, bool) override {
    auto url = request->GetURL().ToString();
    return (frame->IsMain() || url.starts_with("file:")) &&
           !callbacks.allowed(context, url.c_str());
  }
  void OnLoadError(CefRefPtr<CefBrowser>, CefRefPtr<CefFrame> frame,
                   ErrorCode error, const CefString &message,
                   const CefString &) override {
    if (frame->IsMain() && error != ERR_ABORTED)
      fprintf(stderr, "luchs cef navigation: %s\n", message.ToString().c_str());
  }
  IMPLEMENT_REFCOUNTING(Client);
};
void tick() {
  if (closing)
    return;
  if (callbacks.tick(context)) {
    closing = true;
    cleanup(true);
    if (browser)
      browser->GetHost()->CloseBrowser(true);
    else
      CefQuitMessageLoop();
    return;
  }
  CefPostDelayedTask(TID_UI, new Task(tick), 10);
}
class App : public CefApp, public CefBrowserProcessHandler {
public:
  CefRefPtr<CefBrowserProcessHandler> GetBrowserProcessHandler() override {
    return this;
  }
  void OnBeforeCommandLineProcessing(
      const CefString &, CefRefPtr<CefCommandLine> command_line) override {
    // This backend is an isolated spike with a disposable profile. Never ask
    // for the user's real Chromium Safe Storage key during these experiments.
    command_line->AppendSwitch("use-mock-keychain");
    // Casting discovery is unrelated to rendering the page and can trigger
    // macOS Local Network permission prompts even for a local fixture.
    std::string disabled = command_line->GetSwitchValue("disable-features");
    if (!disabled.empty())
      disabled += ",";
    disabled += "MediaRouter,DialMediaRouteProvider";
    command_line->AppendSwitchWithValue("disable-features", disabled);
  }

  void
  OnBeforeChildProcessLaunch(CefRefPtr<CefCommandLine> command_line) override {
    // CEF's subprocess command-line filtering does not preserve this switch.
    // Network/storage services must use the same disposable keychain as us.
    command_line->AppendSwitch("use-mock-keychain");
  }

  void OnContextInitialized() override {
    {
      std::lock_guard lock(dispatch_mutex);
      available = true;
    }
    CefWindowInfo window;
    window.SetAsWindowless(nullptr);
    window.shared_texture_enabled = config.gpu;
    CefBrowserSettings settings;
    settings.windowless_frame_rate = config.fps;
    CefBrowserHost::CreateBrowser(window, new Client, page_url, settings,
                                  nullptr, nullptr);
    CefPostDelayedTask(TID_UI, new Task(tick), 10);
  }
  IMPLEMENT_REFCOUNTING(App);
};
} // namespace
@interface LuchsApplication : NSApplication <CefAppProtocol> {
  BOOL handling_;
}
@end
@implementation LuchsApplication
- (BOOL)isHandlingSendEvent {
  return handling_;
}
- (void)setHandlingSendEvent:(BOOL)value {
  handling_ = value;
}
- (void)sendEvent:(NSEvent *)event {
  CefScopedSendingEvent scoped;
  [super sendEvent:event];
}
- (void)terminate:(id)sender {
  post([] {
    if (browser) {
      closing = true;
      cleanup(true);
      browser->GetHost()->CloseBrowser(true);
    }
  });
}
@end
extern "C" int luchs_cef_run(const LuchsCefConfig *cfg, LuchsCefCallbacks cb,
                             void *ctx) {
  CefScopedLibraryLoader library;
  if (!library.LoadInMain())
    return 1;
  callbacks = cb;
  context = ctx;
  config = *cfg;
  page_url = cfg->url;
  width = cfg->width;
  height = cfg->height;
  @autoreleasepool {
    [LuchsApplication sharedApplication];
    [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
    // Supply these before CEF/Chromium initialization too: early browser
    // services must never see a command line that enables real Keychain IO.
    char keychain[] = "--use-mock-keychain";
    char discovery[] = "--disable-features=MediaRouter,DialMediaRouteProvider";
    char *cef_argv[] = {(*_NSGetArgv())[0], keychain, discovery, nullptr};
    CefMainArgs args(3, cef_argv);
    CefSettings settings;
    CefString(&settings.browser_subprocess_path) =
        [[[NSBundle mainBundle] privateFrameworksPath]
            stringByAppendingPathComponent:
                @"LuchsCEF Helper.app/Contents/MacOS/LuchsCEF Helper"]
            .UTF8String;
    settings.windowless_rendering_enabled = true;
    CefString(&settings.root_cache_path) = cfg->cache;
    CefRefPtr<App> app = new App;
    if (!CefInitialize(args, settings, app, nullptr))
      return CefGetExitCode();
    CefRunMessageLoop();
    {
      std::lock_guard lock(dispatch_mutex);
      available = false;
    }
    CefShutdown();
  }
  context = nullptr;
  return 0;
}
extern "C" int luchs_cef_command(const char *text, uint64_t timeout_ms) {
  @autoreleasepool {
    NSData *data = [NSData dataWithBytes:text length:strlen(text)];
    NSDictionary *command = [NSJSONSerialization JSONObjectWithData:data
                                                            options:0
                                                              error:nil];
    if (![command isKindOfClass:NSDictionary.class])
      return 2;
    std::unique_lock gate(dispatch_mutex);
    if (!available)
      return 2;
    if (CefCurrentlyOn(TID_UI)) {
      gate.unlock();
      return execute(command);
    }
    struct Result {
      std::mutex mutex;
      std::condition_variable cv;
      bool done = false;
      int outcome = 2;
    };
    auto result = std::make_shared<Result>();
    post([command, result] {
      int outcome = execute(command);
      {
        std::lock_guard lock(result->mutex);
        result->outcome = outcome;
        result->done = true;
      }
      result->cv.notify_one();
    });
    gate.unlock();
    std::unique_lock lock(result->mutex);
    if (!result->cv.wait_for(lock, std::chrono::milliseconds(timeout_ms),
                             [&] { return result->done; }))
      return 2;
    return result->outcome;
  }
}
