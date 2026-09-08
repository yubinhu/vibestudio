#include "bindings/bindings.h"
#import <UserNotifications/UserNotifications.h>

// The notification plugin looks up tap metadata in a process-local map and
// force-unwraps it, which crashes when an older notification launches the app.
// VibeStudio uses the plugin for permissions and scheduling, but has no plugin
// notification-event listeners. Handle presentation and taps from OS content.
@interface VibeStudioNotificationDelegate : NSObject <UNUserNotificationCenterDelegate>
+ (void)install;
@end

@implementation VibeStudioNotificationDelegate
+ (void)install {
    // UNUserNotificationCenter keeps its delegate weakly. Retain this instance
    // for the process, and install synchronously after Tauri's plugin is ready.
    static VibeStudioNotificationDelegate *delegate;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        delegate = [[VibeStudioNotificationDelegate alloc] init];
    });
    [UNUserNotificationCenter currentNotificationCenter].delegate = delegate;
}

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
      willPresentNotification:(UNNotification *)notification
        withCompletionHandler:(void (^)(UNNotificationPresentationOptions))completionHandler {
    UNNotificationContent *content = notification.request.content;
    UNNotificationPresentationOptions options =
        UNNotificationPresentationOptionBanner | UNNotificationPresentationOptionList;
    if (content.sound != nil) {
        options |= UNNotificationPresentationOptionSound;
    }
    if (content.badge != nil) {
        options |= UNNotificationPresentationOptionBadge;
    }
    completionHandler(options);
}

- (void)userNotificationCenter:(UNUserNotificationCenter *)center
    didReceiveNotificationResponse:(UNNotificationResponse *)response
             withCompletionHandler:(void (^)(void))completionHandler {
    // iOS opens the app for the default tap. There is no session deep link or
    // action handler to dispatch, and notification metadata need not survive.
    completionHandler();
}
@end

int main(int argc, char * argv[]) {
	ffi::start_app();
	return 0;
}
