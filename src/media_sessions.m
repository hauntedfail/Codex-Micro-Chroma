#import <Foundation/Foundation.h>
#import <dispatch/dispatch.h>
#import <dlfcn.h>
#import <objc/message.h>
#import <objc/runtime.h>

typedef void (*MRGetNowPlayingClients)(dispatch_queue_t, void (^)(id));
typedef void (*MRGetPlayerForClient)(id, id, dispatch_queue_t, void (^)(id));
typedef void (*MRGetInfoForPlayer)(id, BOOL, dispatch_queue_t,
                                   void (^)(NSDictionary *));

@interface MRPlayerPath : NSObject
- (instancetype)initWithOrigin:(id)origin client:(id)client player:(id)player;
@end

@interface MRNowPlayingRequest : NSObject
- (instancetype)initWithPlayerPath:(id)playerPath;
@end

static MRGetNowPlayingClients getNowPlayingClients;
static MRGetPlayerForClient getPlayerForClient;
static MRGetInfoForPlayer getInfoForPlayer;
static dispatch_queue_t requestQueue;
static BOOL refreshInFlight = NO;
static NSData *previousPayloadData = nil;
static NSMutableDictionary<NSString *, NSDictionary *> *artworkCache = nil;

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
    return (long)((int (*)(id, SEL))objc_msgSend)(object, selector);
}

static NSString *cachedArtworkData(NSString *stableID, NSData *artwork) {
    if (!stableID || !artwork) {
        return nil;
    }
    NSDictionary *cached = artworkCache[stableID];
    NSData *cachedData = cached[@"data"];
    NSString *cachedEncoded = cached[@"encoded"];
    if ([cachedData isKindOfClass:[NSData class]] &&
        [cachedEncoded isKindOfClass:[NSString class]] &&
        [cachedData isEqualToData:artwork]) {
        return cachedEncoded;
    }

    NSString *encoded = [artwork base64EncodedStringWithOptions:0];
    artworkCache[stableID] = @{ @"data" : [artwork copy], @"encoded" : encoded };
    return encoded;
}

static void pruneArtworkCache(NSSet<NSString *> *activeStableIDs) {
    for (NSString *stableID in [artworkCache.allKeys copy]) {
        if (![activeStableIDs containsObject:stableID]) {
            [artworkCache removeObjectForKey:stableID];
        }
    }
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
    if ([value isKindOfClass:[NSNumber class]] &&
        isfinite([(NSNumber *)value doubleValue])) {
        destination[outputKey] = value;
    }
}

static NSArray *publicCandidatesIfComplete(NSArray *candidates, BOOL *complete) {
    NSMutableArray *publicCandidates =
        [NSMutableArray arrayWithCapacity:candidates.count];
    BOOL valid = YES;

    for (NSDictionary *candidate in candidates) {
        if (![candidate isKindOfClass:[NSDictionary class]]) {
            valid = NO;
            continue;
        }

        BOOL playing = [candidate[@"playing"] boolValue];
        if (playing) {
            if (![candidate[@"metadataResolved"] boolValue]) {
                valid = NO;
            }
        }

        NSMutableDictionary *publicCandidate = [candidate mutableCopy];
        if ([candidate[@"lastPlayingDateError"] boolValue]) {
            [publicCandidate removeObjectForKey:@"lastPlayingDate"];
        }
        [publicCandidate removeObjectForKey:@"metadataResolved"];
        [publicCandidate removeObjectForKey:@"lastPlayingDateError"];
        [publicCandidates addObject:publicCandidate];
    }

    *complete = valid;
    return publicCandidates;
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
    NSMutableSet<NSString *> *activeStableIDs = [NSMutableSet set];
    Class playerPathClass = NSClassFromString(@"MRPlayerPath");
    Class requestClass = NSClassFromString(@"MRNowPlayingRequest");
    id electedPath = objectProperty(requestClass, @"localNowPlayingPlayerPath");
    __block BOOL completed = NO;
    __block BOOL refreshValid = YES;

    void (^complete)(BOOL) = ^(BOOL timedOut) {
      if (completed) {
          return;
      }
      completed = YES;
      if (!timedOut && refreshValid) {
          BOOL candidatesComplete = NO;
          NSArray *publicCandidates =
              publicCandidatesIfComplete(candidates, &candidatesComplete);
          if (candidatesComplete) {
              printCandidates(publicCandidates);
              pruneArtworkCache(activeStableIDs);
          }
      }
      refreshInFlight = NO;
      scheduleRefresh();
    };

    dispatch_after(
        dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.75 * NSEC_PER_SEC)),
        queue, ^{
          complete(YES);
        });

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
                if (completed) {
                    dispatch_group_leave(group);
                    return;
                }
                if (!player || !playerPathClass) {
                    dispatch_group_leave(group);
                    return;
                }

                MRPlayerPath *playerPath =
                    [[(id)playerPathClass alloc] initWithOrigin:nil
                                                         client:client
                                                         player:player];
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
                [activeStableIDs addObject:stableID];
                NSMutableDictionary *entry = [@{
                    @"stableId" : stableID,
                    @"bundleId" : bundleID,
                    @"playing" : @NO,
                    @"playingResolved" : @NO,
                    @"metadataResolved" : @NO,
                    @"lastPlayingDateError" : @NO,
                    @"elected" : @([playerPath isEqual:electedPath]),
                } mutableCopy];
                [candidates addObject:entry];

                MRNowPlayingRequest *request =
                    requestClass
                        ? [[(id)requestClass alloc] initWithPlayerPath:playerPath]
                        : nil;
                SEL isPlayingSelector = NSSelectorFromString(
                    @"requestIsPlayingOnQueue:completion:");
                BOOL supportsScopedPlaying =
                    request && [request respondsToSelector:isPlayingSelector];

                dispatch_group_enter(group);
                getInfoForPlayer(playerPath, YES, queue,
                                 ^(NSDictionary *information) {
                  if (completed) {
                      dispatch_group_leave(group);
                      return;
                  }
                  if ([information isKindOfClass:[NSDictionary class]]) {
                      entry[@"metadataResolved"] = @YES;
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
                          NSString *encoded =
                              cachedArtworkData(stableID, (NSData *)artwork);
                          if (encoded) {
                              entry[@"artworkData"] = encoded;
                          }
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
                                if (completed) {
                                    dispatch_group_leave(group);
                                    return;
                                }
                                if (error) {
                                    refreshValid = NO;
                                } else {
                                    entry[@"playing"] = @(playing);
                                    entry[@"playingResolved"] = @YES;
                                }
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
                              dispatch_async(queue, ^{
                                if (completed) {
                                    dispatch_group_leave(group);
                                    return;
                                }
                                if (error) {
                                    entry[@"lastPlayingDateError"] = @YES;
                                } else if ([date isKindOfClass:[NSDate class]]) {
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

        dispatch_group_notify(group, queue, ^{
          complete(NO);
        });
    });
}

static void scheduleRefresh(void) {
    dispatch_after(
        dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.25 * NSEC_PER_SEC)),
        dispatch_get_main_queue(), ^{
          refreshSessions();
        });
}

static void printReady(void) {
    printf("{\"ready\":true}\n");
    fflush(stdout);
}

int main(void) {
    @autoreleasepool {
        NSBundle *framework = [NSBundle bundleWithPath:
            @"/System/Library/PrivateFrameworks/MediaRemote.framework"];
        if (![framework load]) {
            fprintf(stderr, "could not load MediaRemote.framework\n");
            return 1;
        }

        getNowPlayingClients = (MRGetNowPlayingClients)dlsym(
            RTLD_DEFAULT, "MRMediaRemoteGetNowPlayingClients");
        getPlayerForClient = (MRGetPlayerForClient)dlsym(
            RTLD_DEFAULT, "MRMediaRemoteGetNowPlayingPlayerForClient");
        getInfoForPlayer = (MRGetInfoForPlayer)dlsym(
            RTLD_DEFAULT, "MRMediaRemoteGetNowPlayingInfoForPlayer");
        if (!getNowPlayingClients || !getPlayerForClient || !getInfoForPlayer) {
            fprintf(stderr, "required per-player MediaRemote symbols are unavailable\n");
            return 1;
        }
        requestQueue = dispatch_queue_create(
            "com.local.codex-micro-chroma.media-sessions", DISPATCH_QUEUE_SERIAL);
        artworkCache = [NSMutableDictionary dictionary];

        Class requestClass = NSClassFromString(@"MRNowPlayingRequest");
        if (!requestClass) {
            fprintf(stderr, "MRNowPlayingRequest is unavailable\n");
            return 1;
        }
        if (!class_getInstanceMethod(
                requestClass,
                NSSelectorFromString(@"requestIsPlayingOnQueue:completion:"))) {
            fprintf(stderr,
                    "MediaRemote scoped playing requests are unavailable; "
                    "playbackRate will be used as the playing fallback\n");
        }
        if (!class_getInstanceMethod(
                requestClass,
                NSSelectorFromString(@"requestLastPlayingDateOnQueue:completion:"))) {
            fprintf(stderr,
                    "MediaRemote lastPlayingDate requests are unavailable; "
                    "OS election will be used as the tie-breaker\n");
        }

        printReady();
        dispatch_after(
            dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.3 * NSEC_PER_SEC)),
            dispatch_get_main_queue(), ^{
              refreshSessions();
            });
        CFRunLoopRun();
    }
    return 0;
}
