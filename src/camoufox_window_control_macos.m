#import <AppKit/AppKit.h>
#import <Foundation/Foundation.h>

#include <dispatch/dispatch.h>
#include <errno.h>
#include <signal.h>
#include <stdlib.h>
#include <string.h>

static NSString *meStatePath;
static NSString *meAcknowledgementPath;
static NSMapTable<NSWindow *, NSDictionary<NSString *, id> *> *meOriginalWindowState;
static dispatch_source_t meStateTimer;
static BOOL meWasInteractive = YES;
static NSInteger meLastReportedWindowCount = -1;
static BOOL meLastReportedInteractive = NO;

static BOOL meIsInteractive(void) {
    NSError *error = nil;
    NSString *state = [NSString stringWithContentsOfFile:meStatePath
                                                encoding:NSUTF8StringEncoding
                                                   error:&error];
    if (error != nil || ![state hasPrefix:@"concealed "]) {
        return YES;
    }
    NSArray<NSString *> *parts = [state componentsSeparatedByCharactersInSet:
        [NSCharacterSet whitespaceAndNewlineCharacterSet]];
    if ([parts count] < 2) {
        return YES;
    }
    pid_t owner = (pid_t)[parts[1] intValue];
    if (owner <= 1) {
        return YES;
    }
    errno = 0;
    return kill(owner, 0) != 0 && errno != EPERM;
}

static void meApplyWindowState(void) {
    BOOL interactive = meIsInteractive();
    NSArray<NSWindow *> *windows = [NSApp windows];
    if (interactive) {
        for (NSWindow *window in meOriginalWindowState) {
            NSDictionary<NSString *, id> *original = [meOriginalWindowState objectForKey:window];
            if (original == nil) {
                continue;
            }
            [window setAlphaValue:[original[@"alpha"] doubleValue]];
            [window setIgnoresMouseEvents:[original[@"ignoresMouse"] boolValue]];
        }
        [meOriginalWindowState removeAllObjects];
        meWasInteractive = YES;
    } else {
        if (meWasInteractive) {
            [meOriginalWindowState removeAllObjects];
            meWasInteractive = NO;
        }
        for (NSWindow *window in windows) {
            if ([meOriginalWindowState objectForKey:window] == nil) {
                [meOriginalWindowState setObject:@{
                    @"alpha": @([window alphaValue]),
                    @"ignoresMouse": @([window ignoresMouseEvents]),
                } forKey:window];
            }
            [window setIgnoresMouseEvents:YES];
            [window setAlphaValue:0.0];
        }
    }

    if ([windows count] > 0 &&
        (meLastReportedWindowCount != (NSInteger)[windows count] ||
         meLastReportedInteractive != interactive)) {
        NSString *acknowledgement = [NSString stringWithFormat:@"%@ %d\n",
            interactive ? @"interactive" : @"concealed", getpid()];
        [acknowledgement writeToFile:meAcknowledgementPath
                          atomically:YES
                            encoding:NSUTF8StringEncoding
                               error:nil];
        meLastReportedWindowCount = (NSInteger)[windows count];
        meLastReportedInteractive = interactive;
    }
}

__attribute__((constructor))
static void meInstallWindowControl(void) {
    const char *program = getprogname();
    const char *statePath = getenv("ME_CAMOUFOX_PRESENTATION_FILE");
    const char *acknowledgementPath = getenv("ME_CAMOUFOX_PRESENTATION_ACK");
    if (program == NULL || strcmp(program, "camoufox") != 0 ||
        statePath == NULL || statePath[0] == '\0' ||
        acknowledgementPath == NULL || acknowledgementPath[0] == '\0') {
        return;
    }
    for (NSString *argument in [[NSProcessInfo processInfo] arguments]) {
        if ([argument isEqualToString:@"-contentproc"]) {
            return;
        }
    }

    meStatePath = [[NSString alloc] initWithUTF8String:statePath];
    meAcknowledgementPath = [[NSString alloc] initWithUTF8String:acknowledgementPath];
    if (meStatePath == nil || meAcknowledgementPath == nil) {
        return;
    }
    meOriginalWindowState = [NSMapTable weakToStrongObjectsMapTable];
    dispatch_async(dispatch_get_main_queue(), ^{
        meStateTimer = dispatch_source_create(
            DISPATCH_SOURCE_TYPE_TIMER,
            0,
            0,
            dispatch_get_main_queue()
        );
        dispatch_source_set_timer(
            meStateTimer,
            dispatch_time(DISPATCH_TIME_NOW, 0),
            25 * NSEC_PER_MSEC,
            5 * NSEC_PER_MSEC
        );
        dispatch_source_set_event_handler(meStateTimer, ^{
            meApplyWindowState();
        });
        dispatch_resume(meStateTimer);
    });
}
