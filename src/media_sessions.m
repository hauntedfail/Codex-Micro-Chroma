#import <Foundation/Foundation.h>
#import <dispatch/dispatch.h>
#import <dlfcn.h>
#import <objc/message.h>
#import <objc/runtime.h>

typedef void (*MRGetNowPlayingClients)(dispatch_queue_t, void (^)(id));
typedef void (*MRGetPlayerForClient)(id, id, dispatch_queue_t, void (^)(id));
typedef void (*MRGetInfoForPlayer)(id, BOOL, dispatch_queue_t,
                                   void (^)(NSDictionary *));

static MRGetNowPlayingClients getNowPlayingClients;
static MRGetPlayerForClient getPlayerForClient;
static MRGetInfoForPlayer getInfoForPlayer;
static dispatch_queue_t requestQueue;
static BOOL refreshInFlight = NO;
static NSData *previousPayloadData = nil;

static id objectProperty(id object, NSString *selectorName) {
    SEL selector = NSSelectorFromString(selectorName);
    if (!object || ![object respondsToSelector:selector]) {
        return nil;
    }
    return ((id(*)(id, SEL))objc_msgSend)(object, selector);
}

static NSString *stringProperty(id object, NSString *selectorName) {
    id value = objectProperty(object, selectorName);
    return [value isKindOfClass:[NSString class]] ? value : nil;
}

static long integerProperty(id object, NSString *selectorName, BOOL *present) {
    SEL selector = NSSelectorFromString(selectorName);
    if (!object || ![object respondsToSelector:selector]) {
        *present = NO;
        return 0;
    }
    *present = YES;
    return ((long(*)(id, SEL))objc_msgSend)(object, selector);
}

static void copyString(NSMutableDictionary *destination, NSString *outputKey,
                       NSDictionary *source, NSString *sourceKey) {
    id value = source[sourceKey];
    if ([value isKindOfClass:[NSString class]]) {
        destination[outputKey] = value;
    }
}

static void copyNumber(NSMutableDictionary *destination, NSString *outputKey,
                       NSDictionary *source, NSString *sourceKey) {
    id value = source[sourceKey];
    if ([value isKindOfClass:[NSNumber class]]) {
        destination[outputKey] = value;
    }
}

static void printCandidates(NSArray *candidates) {
    NSDictionary *payload = @{ @"candidates" : candidates ?: @[] };
    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:payload
                                                   options:0
                                                     error:&error];
    if (!data) {
        fprintf(stderr, "could not serialize MediaRemote sessions: %s\n",
                error.localizedDescription.UTF8String);
        return;
    }
    if ([data isEqualToData:previousPayloadData]) {
        return;
    }
    previousPayloadData = [data copy];
    NSString *line = [[NSString alloc] initWithData:data
                                           encoding:NSUTF8StringEncoding];
    printf("%s\n", line.UTF8String);
    fflush(stdout);
}

static void scheduleRefresh(void);

static void refreshSessions(void) {
    if (refreshInFlight) {
        return;
    }
    refreshInFlight = YES;

    dispatch_queue_t queue = dispatch_get_main_queue();
    NSMutableArray *candidates = [NSMutableArray array];
    Class playerPathClass = NSClassFromString(@"MRPlayerPath");
    Class requestClass = NSClassFromString(@"MRNowPlayingRequest");
    id electedPath = objectProperty(requestClass, @"localNowPlayingPlayerPath");
    __block BOOL completed = NO;

    void (^complete)(void) = ^{
      if (completed) {
          return;
      }
      completed = YES;
      printCandidates(candidates);
      refreshInFlight = NO;
      scheduleRefresh();
    };

    dispatch_after(
        dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.75 * NSEC_PER_SEC)),
        queue, complete);

    getNowPlayingClients(queue, ^(id clientsValue) {
        if (completed) {
            return;
        }
        NSArray *clients = [clientsValue isKindOfClass:[NSArray class]]
                               ? (NSArray *)clientsValue
                               : @[];
        dispatch_group_t group = dispatch_group_create();

        for (id client in clients) {
            dispatch_group_enter(group);
            getPlayerForClient(client, nil, queue, ^(id player) {
                if (!player || !playerPathClass) {
                    dispatch_group_leave(group);
                    return;
                }

                id playerPath = ((id(*)(id, SEL, id, id, id))objc_msgSend)(
                    [playerPathClass alloc],
                    NSSelectorFromString(@"initWithOrigin:client:player:"), nil,
                    client, player);
                if (!playerPath) {
                    dispatch_group_leave(group);
                    return;
                }

                NSString *bundleID =
                    stringProperty(client, @"parentApplicationBundleIdentifier")
                        ?: stringProperty(client, @"bundleIdentifier")
                        ?: @"unknown";
                NSString *playerID = stringProperty(player, @"identifier")
                                         ?: stringProperty(player, @"displayName")
                                         ?: @"default";
                BOOL hasProcessIdentifier = NO;
                long processIdentifier = integerProperty(
                    client, @"processIdentifier", &hasProcessIdentifier);
                NSString *stableID = hasProcessIdentifier
                                         ? [NSString stringWithFormat:@"%@:%@:%ld",
                                                                      bundleID,
                                                                      playerID,
                                                                      processIdentifier]
                                         : [NSString stringWithFormat:@"%@:%@",
                                                                      bundleID,
                                                                      playerID];
                NSMutableDictionary *entry = [@{
                    @"stableId" : stableID,
                    @"bundleId" : bundleID,
                    @"playing" : @NO,
                    @"playingResolved" : @NO,
                    @"elected" : @([playerPath isEqual:electedPath]),
                } mutableCopy];
                [candidates addObject:entry];

                id request = requestClass
                                 ? ((id(*)(id, SEL, id))objc_msgSend)(
                                       [requestClass alloc],
                                       NSSelectorFromString(@"initWithPlayerPath:"),
                                       playerPath)
                                 : nil;
                SEL isPlayingSelector = NSSelectorFromString(
                    @"requestIsPlayingOnQueue:completion:");
                BOOL supportsScopedPlaying =
                    request && [request respondsToSelector:isPlayingSelector];

                dispatch_group_enter(group);
                getInfoForPlayer(playerPath, YES, queue,
                                 ^(NSDictionary *information) {
                  if ([information isKindOfClass:[NSDictionary class]]) {
                      copyString(entry, @"title", information,
                                 @"kMRMediaRemoteNowPlayingInfoTitle");
                      copyString(entry, @"artist", information,
                                 @"kMRMediaRemoteNowPlayingInfoArtist");
                      copyString(entry, @"album", information,
                                 @"kMRMediaRemoteNowPlayingInfoAlbum");
                      copyNumber(entry, @"elapsedTime", information,
                                 @"kMRMediaRemoteNowPlayingInfoElapsedTime");
                      copyNumber(entry, @"duration", information,
                                 @"kMRMediaRemoteNowPlayingInfoDuration");
                      copyNumber(entry, @"playbackRate", information,
                                 @"kMRMediaRemoteNowPlayingInfoPlaybackRate");
                      if (!supportsScopedPlaying) {
                          NSNumber *rate = entry[@"playbackRate"];
                          entry[@"playing"] = @(rate && [rate doubleValue] > 0.0);
                          entry[@"playingResolved"] = @YES;
                      }
                      id timestamp = information[
                          @"kMRMediaRemoteNowPlayingInfoTimestamp"];
                      if ([timestamp isKindOfClass:[NSDate class]]) {
                          entry[@"infoUpdateDate"] =
                              @([(NSDate *)timestamp timeIntervalSince1970]);
                      }
                      id artwork = information[
                          @"kMRMediaRemoteNowPlayingInfoArtworkData"];
                      if ([artwork isKindOfClass:[NSData class]]) {
                          entry[@"artworkData"] =
                              [(NSData *)artwork base64EncodedStringWithOptions:0];
                      }
                  }
                  dispatch_group_leave(group);
                });

                if (request) {
                    if (supportsScopedPlaying) {
                        dispatch_group_enter(group);
                        ((void (*)(id, SEL, dispatch_queue_t,
                                   void (^)(BOOL, NSError *)))objc_msgSend)(
                            request, isPlayingSelector, requestQueue,
                            ^(BOOL playing, NSError *error) {
                              (void)request;
                              dispatch_async(queue, ^{
                                entry[@"playing"] = @(error == nil && playing);
                                entry[@"playingResolved"] = @YES;
                                dispatch_group_leave(group);
                              });
                            });
                    }

                    SEL lastPlayingSelector = NSSelectorFromString(
                        @"requestLastPlayingDateOnQueue:completion:");
                    if (request && [request respondsToSelector:lastPlayingSelector]) {
                        dispatch_group_enter(group);
                        ((void (*)(id, SEL, dispatch_queue_t,
                                   void (^)(NSDate *, NSError *)))objc_msgSend)(
                            request, lastPlayingSelector, requestQueue,
                            ^(NSDate *date, NSError *error) {
                              (void)request;
                              (void)error;
                              dispatch_async(queue, ^{
                                if ([date isKindOfClass:[NSDate class]]) {
                                    entry[@"lastPlayingDate"] =
                                        @([date timeIntervalSince1970]);
                                }
                                dispatch_group_leave(group);
                              });
                            });
                    }
                }

                dispatch_group_leave(group);
            });
        }

        dispatch_group_notify(group, queue, complete);
    });
}

static void scheduleRefresh(void) {
    dispatch_after(
        dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.25 * NSEC_PER_SEC)),
        dispatch_get_main_queue(), ^{
          refreshSessions();
        });
}

__attribute__((visibility("default"))) void chroma_media_sessions_stream(void) {
    @autoreleasepool {
        NSBundle *framework = [NSBundle bundleWithPath:
            @"/System/Library/PrivateFrameworks/MediaRemote.framework"];
        if (![framework load]) {
            fprintf(stderr, "could not load MediaRemote.framework\n");
            return;
        }

        getNowPlayingClients = (MRGetNowPlayingClients)dlsym(
            RTLD_DEFAULT, "MRMediaRemoteGetNowPlayingClients");
        getPlayerForClient = (MRGetPlayerForClient)dlsym(
            RTLD_DEFAULT, "MRMediaRemoteGetNowPlayingPlayerForClient");
        getInfoForPlayer = (MRGetInfoForPlayer)dlsym(
            RTLD_DEFAULT, "MRMediaRemoteGetNowPlayingInfoForPlayer");
        if (!getNowPlayingClients || !getPlayerForClient || !getInfoForPlayer) {
            fprintf(stderr, "required per-player MediaRemote symbols are unavailable\n");
            return;
        }
        requestQueue = dispatch_queue_create(
            "com.local.codex-micro-chroma.media-sessions", DISPATCH_QUEUE_SERIAL);

        Class requestClass = NSClassFromString(@"MRNowPlayingRequest");
        if (!requestClass ||
            !class_getInstanceMethod(
                requestClass,
                NSSelectorFromString(@"requestLastPlayingDateOnQueue:completion:"))) {
            fprintf(stderr,
                    "MediaRemote lastPlayingDate requests are unavailable; "
                    "OS election will be used as the tie-breaker\n");
        }

        dispatch_after(
            dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.3 * NSEC_PER_SEC)),
            dispatch_get_main_queue(), ^{
              refreshSessions();
            });
        CFRunLoopRun();
    }
}
