#import <LocalAuthentication/LocalAuthentication.h>
#import <UIKit/UIKit.h>
#import <mach/mach_time.h>
#include "AppLockPolicy.h"

// This is an accessor lock, not a remote-session lifetime. The callback gates
// the local switchboard and SSH transport; it never stops work on the host.
using VibeStudioAccessCallback = void (*)(bool);

// Unlike wall time (user adjustable) or mach_absolute_time (which excludes
// device sleep), this clock includes suspension and sleep across the grace.
static double VibeStudioContinuousSeconds() {
    static mach_timebase_info_data_t timebase;
    static dispatch_once_t once;
    dispatch_once(&once, ^{ mach_timebase_info(&timebase); });
    return static_cast<double>(mach_continuous_time()) * timebase.numer /
        timebase.denom / 1e9;
}

static UIColor *VibeStudioColor(unsigned int rgb) {
    return [UIColor colorWithRed:((rgb >> 16) & 255) / 255.0
                          green:((rgb >> 8) & 255) / 255.0
                           blue:(rgb & 255) / 255.0 alpha:1];
}

@interface VibeStudioLockViewController : UIViewController
@property(nonatomic, copy) void (^unlockAction)(void);
@property(nonatomic, strong) UILabel *messageLabel;
@property(nonatomic, strong) UIButton *unlockButton;
@property(nonatomic, strong) UILabel *detailLabel;
- (void)showPrivateCover:(BOOL)privateCover authenticating:(BOOL)authenticating
                message:(NSString *)message;
@end

@implementation VibeStudioLockViewController
- (void)loadView {
    self.view = [[UIView alloc] init];
    self.view.backgroundColor = VibeStudioColor(0x101c1e);
    self.view.opaque = YES;
    self.view.accessibilityViewIsModal = YES;
    self.overrideUserInterfaceStyle = UIUserInterfaceStyleDark;

    UIScrollView *scroll = [[UIScrollView alloc] init];
    scroll.translatesAutoresizingMaskIntoConstraints = NO;
    [self.view addSubview:scroll];
    UIView *content = [[UIView alloc] init];
    content.translatesAutoresizingMaskIntoConstraints = NO;
    [scroll addSubview:content];

    UIImageView *mark = [[UIImageView alloc] initWithImage:
        [UIImage systemImageNamed:@"lock.fill"]];
    mark.tintColor = VibeStudioColor(0xd2f16a);
    mark.contentMode = UIViewContentModeScaleAspectFit;
    mark.isAccessibilityElement = NO;
    [mark.heightAnchor constraintEqualToConstant:44].active = YES;

    UILabel *title = [[UILabel alloc] init];
    title.text = @"VibeStudio";
    title.font = [UIFont preferredFontForTextStyle:UIFontTextStyleLargeTitle];
    title.adjustsFontForContentSizeCategory = YES;
    title.textColor = VibeStudioColor(0xeff4eb);
    title.textAlignment = NSTextAlignmentCenter;
    title.numberOfLines = 0;
    title.accessibilityTraits = UIAccessibilityTraitHeader;

    self.messageLabel = [[UILabel alloc] init];
    self.messageLabel.font = [UIFont preferredFontForTextStyle:UIFontTextStyleBody];
    self.messageLabel.adjustsFontForContentSizeCategory = YES;
    self.messageLabel.textColor = VibeStudioColor(0xa9b6b0);
    self.messageLabel.textAlignment = NSTextAlignmentCenter;
    self.messageLabel.numberOfLines = 0;

    self.unlockButton = [UIButton buttonWithType:UIButtonTypeSystem];
    [self.unlockButton setTitle:@"Unlock" forState:UIControlStateNormal];
    [self.unlockButton setTitleColor:VibeStudioColor(0x152b2f) forState:UIControlStateNormal];
    self.unlockButton.backgroundColor = VibeStudioColor(0xd2f16a);
    self.unlockButton.layer.cornerRadius = 12;
    self.unlockButton.titleLabel.font = [UIFont preferredFontForTextStyle:UIFontTextStyleHeadline];
    self.unlockButton.titleLabel.adjustsFontForContentSizeCategory = YES;
    self.unlockButton.contentEdgeInsets = UIEdgeInsetsMake(14, 24, 14, 24);
    self.unlockButton.accessibilityHint = @"Authenticate with Face ID, Touch ID, or your device passcode.";
    [self.unlockButton.heightAnchor constraintGreaterThanOrEqualToConstant:52].active = YES;
    [self.unlockButton addTarget:self action:@selector(unlock) forControlEvents:UIControlEventTouchUpInside];

    self.detailLabel = [[UILabel alloc] init];
    self.detailLabel.text = @"Locks after 1 minute away.";
    self.detailLabel.font = [UIFont preferredFontForTextStyle:UIFontTextStyleFootnote];
    self.detailLabel.adjustsFontForContentSizeCategory = YES;
    self.detailLabel.textColor = VibeStudioColor(0xa9b6b0);
    self.detailLabel.textAlignment = NSTextAlignmentCenter;
    self.detailLabel.numberOfLines = 0;

    UIStackView *stack = [[UIStackView alloc] initWithArrangedSubviews:
        @[mark, title, self.messageLabel, self.unlockButton, self.detailLabel]];
    stack.axis = UILayoutConstraintAxisVertical;
    stack.spacing = 20;
    stack.translatesAutoresizingMaskIntoConstraints = NO;
    [content addSubview:stack];
    NSLayoutConstraint *width = [stack.widthAnchor constraintEqualToAnchor:content.widthAnchor constant:-48];
    width.priority = UILayoutPriorityDefaultHigh;
    [NSLayoutConstraint activateConstraints:@[
        [scroll.leadingAnchor constraintEqualToAnchor:self.view.safeAreaLayoutGuide.leadingAnchor],
        [scroll.trailingAnchor constraintEqualToAnchor:self.view.safeAreaLayoutGuide.trailingAnchor],
        [scroll.topAnchor constraintEqualToAnchor:self.view.safeAreaLayoutGuide.topAnchor],
        [scroll.bottomAnchor constraintEqualToAnchor:self.view.safeAreaLayoutGuide.bottomAnchor],
        [content.leadingAnchor constraintEqualToAnchor:scroll.contentLayoutGuide.leadingAnchor],
        [content.trailingAnchor constraintEqualToAnchor:scroll.contentLayoutGuide.trailingAnchor],
        [content.topAnchor constraintEqualToAnchor:scroll.contentLayoutGuide.topAnchor],
        [content.bottomAnchor constraintEqualToAnchor:scroll.contentLayoutGuide.bottomAnchor],
        [content.widthAnchor constraintEqualToAnchor:scroll.frameLayoutGuide.widthAnchor],
        [content.heightAnchor constraintGreaterThanOrEqualToAnchor:scroll.frameLayoutGuide.heightAnchor],
        [stack.centerXAnchor constraintEqualToAnchor:content.centerXAnchor],
        [stack.centerYAnchor constraintEqualToAnchor:content.centerYAnchor],
        [stack.topAnchor constraintGreaterThanOrEqualToAnchor:content.topAnchor constant:24],
        [stack.bottomAnchor constraintLessThanOrEqualToAnchor:content.bottomAnchor constant:-24],
        [stack.widthAnchor constraintLessThanOrEqualToConstant:360],
        width,
    ]];
}

- (UIStatusBarStyle)preferredStatusBarStyle { return UIStatusBarStyleLightContent; }
- (void)unlock { if (self.unlockAction) self.unlockAction(); }

- (void)showPrivateCover:(BOOL)privateCover authenticating:(BOOL)authenticating
                message:(NSString *)message {
    [self loadViewIfNeeded];
    self.messageLabel.hidden = privateCover;
    self.unlockButton.hidden = privateCover;
    self.detailLabel.hidden = privateCover;
    self.messageLabel.text = authenticating ? @"Unlocking…" : (message ?: @"Unlock to access your computer.");
    self.unlockButton.enabled = !authenticating;
    self.unlockButton.alpha = authenticating ? 0.6 : 1;
}
@end

@interface VibeStudioAppLock : NSObject {
    VibeStudioAccessCallback _setUnlocked;
    VibeStudioLockPolicy _policy;
    uint64_t _backgroundGeneration;
    BOOL _automaticPromptOnActive;
    LAContext *_authenticationContext;
    UIWindow *_coverWindow;
    __weak UIWindow *_contentWindow;
    BOOL _contentAccessibilityCaptured;
    BOOL _contentAccessibilityWasHidden;
    VibeStudioLockViewController *_coverController;
    NSString *_message;
}
+ (void)installWithCallback:(void *)callback;
+ (void)refresh;
@end

static VibeStudioAppLock *VibeStudioInstalledAppLock;

@implementation VibeStudioAppLock
+ (void)installWithCallback:(void *)callback {
    NSAssert([NSThread isMainThread], @"Install the app lock on the main thread.");
    // Setup is once per process. Never reset a lock's grace or authentication
    // state if a caller accidentally repeats installation.
    if (VibeStudioInstalledAppLock) return;
    VibeStudioInstalledAppLock = [[self alloc] init];
    [VibeStudioInstalledAppLock install:reinterpret_cast<VibeStudioAccessCallback>(callback)];
}

+ (void)refresh {
    NSAssert([NSThread isMainThread], @"Refresh the app lock on the main thread.");
    [VibeStudioInstalledAppLock expireGraceIfNeeded];
}

- (void)install:(VibeStudioAccessCallback)callback {
    _setUnlocked = callback;
    _automaticPromptOnActive = YES;
    if (_setUnlocked) _setUnlocked(false);
    NSNotificationCenter *center = [NSNotificationCenter defaultCenter];
    [center addObserver:self selector:@selector(willResignActive:)
                   name:UIApplicationWillResignActiveNotification object:nil];
    [center addObserver:self selector:@selector(didEnterBackground:)
                   name:UIApplicationDidEnterBackgroundNotification object:nil];
    [center addObserver:self selector:@selector(willEnterForeground:)
                   name:UIApplicationWillEnterForegroundNotification object:nil];
    [center addObserver:self selector:@selector(didBecomeActive:)
                   name:UIApplicationDidBecomeActiveNotification object:nil];
    [self showCover];
    if ([UIApplication sharedApplication].applicationState == UIApplicationStateActive) {
        [self didBecomeActive:nil];
    }
}

- (void)ensureCover {
    if (_coverWindow) return;
    UIApplication *application = [UIApplication sharedApplication];
    // Tauri currently uses one application window, without a scene manifest.
    // Respect its scene when present so this also works with a future scene host.
    for (UIWindow *window in application.windows) {
        if (window.isKeyWindow) { _contentWindow = window; break; }
    }
    if (_contentWindow.windowScene) {
        _coverWindow = [[UIWindow alloc] initWithWindowScene:_contentWindow.windowScene];
    } else {
        _coverWindow = [[UIWindow alloc] initWithFrame:[UIScreen mainScreen].bounds];
    }
    _coverWindow.windowLevel = UIWindowLevelAlert + 1;
    _coverWindow.backgroundColor = VibeStudioColor(0x101c1e);
    _coverWindow.opaque = YES;
    _coverController = [[VibeStudioLockViewController alloc] init];
    __weak VibeStudioAppLock *weakSelf = self;
    _coverController.unlockAction = ^{ [weakSelf authenticate]; };
    _coverWindow.rootViewController = _coverController;
}

- (void)showCover {
    [self ensureCover];
    // accessibilityViewIsModal only excludes siblings within the cover's own
    // window. Also hide the retained workspace window from VoiceOver while the
    // opaque cover is visible, including short, transient inactive periods.
    if (_contentWindow && !_contentAccessibilityCaptured) {
        _contentAccessibilityWasHidden = _contentWindow.accessibilityElementsHidden;
        _contentAccessibilityCaptured = YES;
        _contentWindow.accessibilityElementsHidden = YES;
    }
    [_coverController showPrivateCover:!_policy.active() authenticating:_policy.authenticating() message:_message];
    _coverWindow.hidden = NO;
    // A transient privacy cover does not steal keyboard focus from the terminal.
    if (_policy.active() && !_policy.unlocked()) [_coverWindow makeKeyWindow];
    [_coverController.view layoutIfNeeded];
}

- (void)revealUnlockedContent {
    if (!_policy.unlocked() || !_policy.active() || _policy.backgrounded()) return;
    // Repeat true for every active grace return: the Rust client may need to
    // heal a suspended listener/tunnel. Restore access before revealing content.
    if (_setUnlocked) _setUnlocked(true);
    _coverWindow.hidden = YES;
    if (_contentAccessibilityCaptured) {
        _contentWindow.accessibilityElementsHidden = _contentAccessibilityWasHidden;
        _contentAccessibilityCaptured = NO;
    }
    if (_contentWindow && !_contentWindow.isKeyWindow) [_contentWindow makeKeyWindow];
    UIAccessibilityPostNotification(UIAccessibilityScreenChangedNotification, nil);
}

- (void)expireGraceIfNeeded {
    if (_policy.refresh(VibeStudioContinuousSeconds())) {
        if (_setUnlocked) _setUnlocked(false);
    }
}

- (void)willResignActive:(NSNotification *)notification {
    _policy.resignActive();
    // App-switcher snapshots are covered immediately, including transient
    // system interruptions. Only DidEnterBackground starts the grace clock.
    [self showCover];
}

- (void)didEnterBackground:(NSNotification *)notification {
    _policy.enterBackground(VibeStudioContinuousSeconds());
    _backgroundGeneration++;
    [_authenticationContext invalidate];
    _authenticationContext = nil;
    _message = nil;
    [self showCover];
    const uint64_t generation = _backgroundGeneration;
    // Best effort while iOS permits execution. Foreground and +refresh always
    // recheck the continuous clock; correctness never depends on this timer.
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(VibeStudioLockPolicy::graceSeconds * NSEC_PER_SEC)),
                   dispatch_get_main_queue(), ^{
        if (self->_policy.backgrounded() && self->_backgroundGeneration == generation) {
            [self expireGraceIfNeeded];
        }
    });
}

- (void)willEnterForeground:(NSNotification *)notification {
    if (_policy.enterForeground(VibeStudioContinuousSeconds()) && _setUnlocked) _setUnlocked(false);
    _backgroundGeneration++;
    _automaticPromptOnActive = !_policy.unlocked();
    [self showCover];
}

- (void)didBecomeActive:(NSNotification *)notification {
    // Defensive fallback if a host ever omits WillEnterForeground.
    if (_policy.backgrounded()) [self willEnterForeground:nil];
    _policy.becomeActive();
    if (_policy.unlocked()) {
        [self revealUnlockedContent];
    } else {
        [self showCover];
        if (_automaticPromptOnActive && !_policy.authenticating()) {
            dispatch_async(dispatch_get_main_queue(), ^{
                if (self->_policy.active() && self->_automaticPromptOnActive) [self authenticate];
            });
        }
    }
}

- (void)authenticate {
    if (!_policy.active() || _policy.backgrounded() || _policy.unlocked() || _policy.authenticating()) return;
    _automaticPromptOnActive = NO;
    _message = nil;
    LAContext *context = [[LAContext alloc] init];
    context.localizedCancelTitle = @"Cancel";
    NSError *availabilityError = nil;
    if (![context canEvaluatePolicy:LAPolicyDeviceOwnerAuthentication error:&availabilityError]) {
        _message = availabilityError.code == LAErrorPasscodeNotSet
            ? @"Set up a device passcode in Settings to unlock VibeStudio."
            : @"Device authentication is unavailable. Try again to unlock VibeStudio.";
        [self showCover];
        UIAccessibilityPostNotification(UIAccessibilityAnnouncementNotification, _message);
        return;
    }
    _authenticationContext = context;
    const uint64_t generation = _policy.beginAuthentication();
    [self showCover];
    [context evaluatePolicy:LAPolicyDeviceOwnerAuthentication
            localizedReason:@"Unlock your remote computer sessions."
                      reply:^(BOOL success, NSError *error) {
        dispatch_async(dispatch_get_main_queue(), ^{
            // A background event invalidates even a successful late reply.
            auto result = self->_policy.finishAuthentication(generation, success);
            if (result == VibeStudioLockPolicy::AuthenticationResult::ignored) return;
            self->_authenticationContext = nil;
            if (result == VibeStudioLockPolicy::AuthenticationResult::unlocked) {
                [self revealUnlockedContent];
            } else if (result == VibeStudioLockPolicy::AuthenticationResult::failed) {
                self->_message = (error.code == LAErrorUserCancel || error.code == LAErrorSystemCancel ||
                                  error.code == LAErrorAppCancel)
                    ? @"VibeStudio is locked. Tap Unlock to try again."
                    : @"Couldn’t verify your identity. Tap Unlock to try again.";
                [self showCover];
                if (self->_policy.active()) UIAccessibilityPostNotification(UIAccessibilityAnnouncementNotification, self->_message);
            }
        });
    }];
}
@end

// Kept as a C ABI entry point for native hosts. Rust's Tauri cdylib resolves
// +installWithCallback: at runtime because Xcode links this object afterward.
extern "C" void vibestudio_app_lock_install(VibeStudioAccessCallback set_unlocked) {
    [VibeStudioAppLock installWithCallback:reinterpret_cast<void *>(set_unlocked)];
}
