#import <Foundation/Foundation.h>
#import <dispatch/dispatch.h>
#import <dlfcn.h>
#import <errno.h>
#import <objc/message.h>
#import <objc/runtime.h>
#import <string.h>
#import <unistd.h>

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
static NSUInteger artworkCacheBytes = 0;
static char **helperArgv = NULL;

// MediaRemote is local but untrusted input. These caps cover ordinary album art
// while bounding copied payload text, retained artwork, and JSON size.
static const NSUInteger MAX_TEXT_FIELD_BYTES = 8 * 1024;
static const NSUInteger MAX_RAW_ARTWORK_BYTES = 8 * 1024 * 1024;
static const NSUInteger MAX_SERIALIZED_ARTWORK_BYTES = 16 * 1024 * 1024;
static const NSUInteger MAX_ARTWORK_CACHE_BYTES = 32 * 1024 * 1024;
static const NSUInteger MAX_ENRICHED_PLAYING_CANDIDATES = 8;
static const NSUInteger MAX_PHASE1_BATCH_CLIENTS = 8;

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

static NSString *boundedString(NSString *value) {
    if (!value) {
        return nil;
    }

    NSUInteger valueLength = value.length;
    if (valueLength == 0) {
        return value;
    }

    NSMutableData *scratch = [NSMutableData dataWithLength:MAX_TEXT_FIELD_BYTES];
    NSUInteger fullUsedLength = 0;
    NSRange remainingRange = NSMakeRange(0, 0);
    BOOL fullConverted = [value getBytes:scratch.mutableBytes
                              maxLength:MAX_TEXT_FIELD_BYTES
                             usedLength:&fullUsedLength
                               encoding:NSUTF8StringEncoding
                                options:0
                                  range:NSMakeRange(0, valueLength)
                         remainingRange:&remainingRange];
    if (fullConverted && fullUsedLength <= MAX_TEXT_FIELD_BYTES &&
        remainingRange.location == NSMaxRange(NSMakeRange(0, valueLength))) {
        return value;
    }

    NSUInteger low = 0;
    NSUInteger high = valueLength;
    NSUInteger bestLength = 0;
    NSUInteger bestBytes = 0;
    while (low <= high) {
        NSUInteger mid = low + (high - low) / 2;
        NSUInteger usedLength = 0;
        remainingRange = NSMakeRange(0, 0);
        BOOL converted = [value getBytes:scratch.mutableBytes
                               maxLength:MAX_TEXT_FIELD_BYTES
                              usedLength:&usedLength
                                encoding:NSUTF8StringEncoding
                                 options:0
                                   range:NSMakeRange(0, mid)
                          remainingRange:&remainingRange];
        if (converted && usedLength <= MAX_TEXT_FIELD_BYTES &&
            remainingRange.location == mid) {
            bestLength = mid;
            bestBytes = usedLength;
            if (mid == valueLength) {
                break;
            }
            low = mid + 1;
        } else if (mid == 0) {
            break;
        } else {
            high = mid - 1;
        }
    }
    if (bestLength == 0 && bestBytes == 0) {
        return @"";
    }

    NSMutableData *data = [NSMutableData dataWithLength:bestBytes];
    NSUInteger usedLength = 0;
    BOOL converted = [value getBytes:data.mutableBytes
                           maxLength:bestBytes
                          usedLength:&usedLength
                            encoding:NSUTF8StringEncoding
                             options:0
                               range:NSMakeRange(0, bestLength)
                      remainingRange:&remainingRange];
    if (!converted || usedLength != bestBytes) {
        return @"";
    }
    return [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding] ?: @"";
}

static BOOL addWouldExceedNSUInteger(NSUInteger left, NSUInteger right,
                                     NSUInteger limit) {
    return left > limit || right > limit - left;
}

static NSUInteger artworkCacheCost(NSData *data, NSString *encoded) {
    if (!data || !encoded) {
        return 0;
    }

    NSUInteger encodedBytes =
        [encoded lengthOfBytesUsingEncoding:NSUTF8StringEncoding];
    if (addWouldExceedNSUInteger(data.length, encodedBytes, NSUIntegerMax)) {
        return NSUIntegerMax;
    }
    return data.length + encodedBytes;
}

static NSUInteger cachedArtworkEntryCost(NSDictionary *entry) {
    NSData *data = entry[@"data"];
    NSString *encoded = entry[@"encoded"];
    if (![data isKindOfClass:[NSData class]] ||
        ![encoded isKindOfClass:[NSString class]]) {
        return 0;
    }
    return artworkCacheCost(data, encoded);
}

static void removeCachedArtwork(NSString *stableID) {
    NSDictionary *entry = artworkCache[stableID];
    if (!entry) {
        return;
    }

    NSUInteger cost = cachedArtworkEntryCost(entry);
    artworkCacheBytes = cost > artworkCacheBytes ? 0 : artworkCacheBytes - cost;
    [artworkCache removeObjectForKey:stableID];
}

static NSString *cachedArtworkData(NSString *stableID, NSData *artwork) {
    if (!stableID || !artwork) {
        return nil;
    }
    if (artwork.length > MAX_RAW_ARTWORK_BYTES) {
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
    removeCachedArtwork(stableID);
    NSUInteger cost = artworkCacheCost(artwork, encoded);
    if (addWouldExceedNSUInteger(artworkCacheBytes, cost,
                                 MAX_ARTWORK_CACHE_BYTES)) {
        return encoded;
    }

    artworkCache[stableID] = @{ @"data" : [artwork copy], @"encoded" : encoded };
    artworkCacheBytes += cost;
    return encoded;
}

static void pruneArtworkCache(NSSet<NSString *> *activeStableIDs) {
    for (NSString *stableID in [artworkCache.allKeys copy]) {
        if (![activeStableIDs containsObject:stableID]) {
            removeCachedArtwork(stableID);
        }
    }
}

static void copyString(NSMutableDictionary *destination, NSString *outputKey,
                       NSDictionary *source, NSString *sourceKey) {
    id value = source[sourceKey];
    if ([value isKindOfClass:[NSString class]]) {
        destination[outputKey] = boundedString((NSString *)value);
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

static void copyDateSeconds(NSMutableDictionary *destination, NSString *outputKey,
                            NSDate *date) {
    if (![date isKindOfClass:[NSDate class]]) {
        return;
    }
    NSTimeInterval seconds = [date timeIntervalSince1970];
    if (isfinite(seconds)) {
        destination[outputKey] = @(seconds);
    }
}

static void copyMetadata(NSMutableDictionary *entry, NSDictionary *information,
                         NSString *stableID, BOOL includeArtwork,
                         BOOL resolvePlayingFromRate) {
    if (![information isKindOfClass:[NSDictionary class]]) {
        return;
    }
    copyString(entry, @"title", information, @"kMRMediaRemoteNowPlayingInfoTitle");
    copyString(entry, @"artist", information, @"kMRMediaRemoteNowPlayingInfoArtist");
    copyString(entry, @"album", information, @"kMRMediaRemoteNowPlayingInfoAlbum");
    copyNumber(entry, @"elapsedTime", information,
               @"kMRMediaRemoteNowPlayingInfoElapsedTime");
    copyNumber(entry, @"duration", information,
               @"kMRMediaRemoteNowPlayingInfoDuration");
    copyNumber(entry, @"playbackRate", information,
               @"kMRMediaRemoteNowPlayingInfoPlaybackRate");
    if (resolvePlayingFromRate) {
        NSNumber *rate = entry[@"playbackRate"];
        entry[@"playing"] = @(rate && [rate doubleValue] > 0.0);
        entry[@"playingResolved"] = @YES;
    }
    copyDateSeconds(entry, @"infoUpdateDate",
                    information[@"kMRMediaRemoteNowPlayingInfoTimestamp"]);
    if (!includeArtwork) {
        return;
    }
    id artwork = information[@"kMRMediaRemoteNowPlayingInfoArtworkData"];
    if ([artwork isKindOfClass:[NSData class]]) {
        NSString *encoded = cachedArtworkData(stableID, (NSData *)artwork);
        if (encoded) {
            entry[@"artworkData"] = encoded;
        }
    }
}

static NSArray *publicCandidates(NSArray *candidates) {
    NSMutableArray *publicCandidates =
        [NSMutableArray arrayWithCapacity:candidates.count];
    NSUInteger serializedArtworkBytes = 0;

    for (NSDictionary *candidate in candidates) {
        if (![candidate isKindOfClass:[NSDictionary class]]) {
            continue;
        }

        NSMutableDictionary *publicCandidate = [candidate mutableCopy];
        BOOL playing = [candidate[@"playing"] boolValue];
        BOOL playingResolved = [candidate[@"playingResolved"] boolValue];
        NSString *artworkData = publicCandidate[@"artworkData"];
        if ([artworkData isKindOfClass:[NSString class]]) {
            NSUInteger artworkBytes =
                [artworkData lengthOfBytesUsingEncoding:NSUTF8StringEncoding];
            if (!playing || !playingResolved ||
                artworkBytes > MAX_SERIALIZED_ARTWORK_BYTES ||
                serializedArtworkBytes + artworkBytes > MAX_SERIALIZED_ARTWORK_BYTES) {
                [publicCandidate removeObjectForKey:@"artworkData"];
            } else {
                serializedArtworkBytes += artworkBytes;
            }
        }
        if ([candidate[@"lastPlayingDateError"] boolValue]) {
            [publicCandidate removeObjectForKey:@"lastPlayingDate"];
        }
        [publicCandidate removeObjectForKey:@"lastPlayingDateError"];
        [publicCandidates addObject:publicCandidate];
    }

    return publicCandidates;
}

static NSComparisonResult compareCandidateRank(NSDictionary *left,
                                               NSDictionary *right) {
    NSNumber *leftDate = left[@"lastPlayingDate"];
    NSNumber *rightDate = right[@"lastPlayingDate"];
    BOOL leftHasDate = [leftDate isKindOfClass:[NSNumber class]];
    BOOL rightHasDate = [rightDate isKindOfClass:[NSNumber class]];
    if (leftHasDate && rightHasDate) {
        double leftValue = [leftDate doubleValue];
        double rightValue = [rightDate doubleValue];
        if (leftValue > rightValue) {
            return NSOrderedAscending;
        }
        if (leftValue < rightValue) {
            return NSOrderedDescending;
        }
    } else if (leftHasDate) {
        return NSOrderedAscending;
    } else if (rightHasDate) {
        return NSOrderedDescending;
    }

    BOOL leftElected = [left[@"elected"] boolValue];
    BOOL rightElected = [right[@"elected"] boolValue];
    if (leftElected != rightElected) {
        return leftElected ? NSOrderedAscending : NSOrderedDescending;
    }

    NSString *leftStableID = left[@"stableId"];
    NSString *rightStableID = right[@"stableId"];
    if (![leftStableID isKindOfClass:[NSString class]]) {
        leftStableID = @"";
    }
    if (![rightStableID isKindOfClass:[NSString class]]) {
        rightStableID = @"";
    }
    return [leftStableID compare:rightStableID];
}

#if defined(MEDIA_SESSIONS_SMOKE)
static NSArray *rankedPlayingCandidates(NSArray *candidates, NSUInteger limit) {
    NSMutableArray *playingCandidates = [NSMutableArray array];
    for (NSDictionary *candidate in candidates) {
        if (![candidate isKindOfClass:[NSDictionary class]]) {
            continue;
        }
        if ([candidate[@"playing"] boolValue] &&
            [candidate[@"playingResolved"] boolValue]) {
            [playingCandidates addObject:candidate];
        }
    }
    [playingCandidates sortUsingComparator:^NSComparisonResult(NSDictionary *left,
                                                               NSDictionary *right) {
      return compareCandidateRank(left, right);
    }];
    if (playingCandidates.count > limit) {
        return [playingCandidates subarrayWithRange:NSMakeRange(0, limit)];
    }
    return playingCandidates;
}
#endif

static NSComparisonResult compareRecordRank(NSDictionary *left,
                                            NSDictionary *right) {
    NSDictionary *leftEntry = left[@"entry"];
    NSDictionary *rightEntry = right[@"entry"];
    return compareCandidateRank(leftEntry, rightEntry);
}

static void insertTopRecord(NSMutableArray *topRecords, NSDictionary *record,
                            NSUInteger limit) {
    NSDictionary *entry = record[@"entry"];
    if (![entry isKindOfClass:[NSDictionary class]] ||
        ![entry[@"playing"] boolValue] ||
        ![entry[@"playingResolved"] boolValue]) {
        return;
    }
    [topRecords addObject:record];
    [topRecords sortUsingComparator:^NSComparisonResult(NSDictionary *left,
                                                        NSDictionary *right) {
      return compareRecordRank(left, right);
    }];
    while (topRecords.count > limit) {
        [topRecords removeLastObject];
    }
}

static NSArray *candidateEntriesFromRecords(NSArray *records) {
    NSMutableArray *entries = [NSMutableArray arrayWithCapacity:records.count];
    for (NSDictionary *record in records) {
        NSDictionary *entry = record[@"entry"];
        if ([entry isKindOfClass:[NSDictionary class]]) {
            [entries addObject:entry];
        }
    }
    return entries;
}

static NSUInteger nextWatchdogToken(NSUInteger token) {
    return token == NSUIntegerMax ? 1 : token + 1;
}

static BOOL watchdogTokenIsCurrent(NSUInteger armedToken,
                                   NSUInteger currentToken) {
    return armedToken != 0 && armedToken == currentToken;
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

static void restartAfterTimeout(void) {
    printf("{\"reset\":\"timeout\"}\n");
    fflush(stdout);
    if (!helperArgv || !helperArgv[0]) {
        fprintf(stderr, "could not restart MediaRemote session helper after timeout: missing executable path\n");
        _exit(70);
    }
    execv(helperArgv[0], helperArgv);
    fprintf(stderr,
            "could not restart MediaRemote session helper after timeout via execv(%s): %s\n",
            helperArgv[0], strerror(errno));
    _exit(70);
}

static void refreshSessions(void) {
    if (refreshInFlight) {
        return;
    }
    refreshInFlight = YES;

    dispatch_queue_t queue = dispatch_get_main_queue();
    NSMutableArray *topRecords = [NSMutableArray array];
    Class playerPathClass = NSClassFromString(@"MRPlayerPath");
    Class requestClass = NSClassFromString(@"MRNowPlayingRequest");
    id electedPath = objectProperty(requestClass, @"localNowPlayingPlayerPath");
    __block BOOL completed = NO;
    __block NSUInteger watchdogToken = 0;
    __block void (^complete)(BOOL) = nil;

    void (^armWatchdog)(void) = ^{
      watchdogToken = nextWatchdogToken(watchdogToken);
      NSUInteger armedToken = watchdogToken;
      dispatch_after(
          dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.75 * NSEC_PER_SEC)),
          queue, ^{
            if (!completed &&
                watchdogTokenIsCurrent(armedToken, watchdogToken)) {
                complete(YES);
            }
          });
    };

    complete = ^(BOOL timedOut) {
      if (completed) {
          return;
      }
      completed = YES;
      watchdogToken = nextWatchdogToken(watchdogToken);
      if (timedOut) {
          restartAfterTimeout();
      } else {
          NSArray *candidates = publicCandidates(candidateEntriesFromRecords(topRecords));
          printCandidates(candidates);
          NSMutableSet<NSString *> *activeStableIDs =
              [NSMutableSet setWithCapacity:candidates.count];
          for (NSDictionary *candidate in candidates) {
              NSString *stableID = candidate[@"stableId"];
              if ([stableID isKindOfClass:[NSString class]]) {
                  [activeStableIDs addObject:stableID];
              }
          }
          pruneArtworkCache(activeStableIDs);
      }
      refreshInFlight = NO;
      scheduleRefresh();
    };

    armWatchdog();
    getNowPlayingClients(queue, ^(id clientsValue) {
        if (completed) {
            return;
        }
        NSArray *clients = [clientsValue isKindOfClass:[NSArray class]]
                               ? (NSArray *)clientsValue
                               : @[];
        __block void (^enrichTopRecords)(void);
        __block void (^processBatch)(NSUInteger) = nil;

        enrichTopRecords = ^{
          if (completed || topRecords.count == 0) {
              complete(NO);
              return;
          }
          armWatchdog();
          dispatch_group_t enrichmentGroup = dispatch_group_create();
          for (NSDictionary *record in [topRecords copy]) {
              NSMutableDictionary *entry = record[@"entry"];
              id playerPath = record[@"playerPath"];
              NSString *stableID = entry[@"stableId"];
              if (!playerPath || ![stableID isKindOfClass:[NSString class]]) {
                  continue;
              }
              dispatch_group_enter(enrichmentGroup);
              getInfoForPlayer(playerPath, YES, queue,
                               ^(NSDictionary *information) {
                if (!completed) {
                    copyMetadata(entry, information, stableID, YES, NO);
                }
                dispatch_group_leave(enrichmentGroup);
              });
          }
          dispatch_group_notify(enrichmentGroup, queue, ^{
            complete(NO);
          });
        };

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Warc-retain-cycles"
        processBatch = ^(NSUInteger startIndex) {
          if (completed) {
              processBatch = nil;
              return;
          }
          if (startIndex >= clients.count) {
              enrichTopRecords();
              processBatch = nil;
              return;
          }

          armWatchdog();
          NSUInteger endIndex =
              MIN(clients.count, startIndex + MAX_PHASE1_BATCH_CLIENTS);
          dispatch_group_t batchGroup = dispatch_group_create();
          NSMutableArray *batchRecords =
              [NSMutableArray arrayWithCapacity:endIndex - startIndex];

          for (NSUInteger index = startIndex; index < endIndex; index++) {
              id client = clients[index];
              dispatch_group_enter(batchGroup);
              getPlayerForClient(client, nil, queue, ^(id player) {
                if (completed) {
                    dispatch_group_leave(batchGroup);
                    return;
                }
                if (!player || !playerPathClass) {
                    dispatch_group_leave(batchGroup);
                    return;
                }

                MRPlayerPath *playerPath =
                    [[(id)playerPathClass alloc] initWithOrigin:nil
                                                         client:client
                                                         player:player];
                if (!playerPath) {
                    dispatch_group_leave(batchGroup);
                    return;
                }

                NSString *bundleID = boundedString(
                    stringProperty(client, @"parentApplicationBundleIdentifier")
                        ?: stringProperty(client, @"bundleIdentifier")
                        ?: @"unknown");
                NSString *playerID =
                    boundedString(stringProperty(player, @"identifier")
                                  ?: stringProperty(player, @"displayName")
                                  ?: @"default");
                BOOL hasProcessIdentifier = NO;
                long processIdentifier =
                    integerProperty(client, @"processIdentifier",
                                    &hasProcessIdentifier);
                NSString *stableID =
                    hasProcessIdentifier
                        ? [NSString stringWithFormat:@"%@:%@:%ld", bundleID,
                                                   playerID, processIdentifier]
                        : [NSString stringWithFormat:@"%@:%@", bundleID, playerID];
                NSMutableDictionary *entry = [@{
                    @"stableId" : stableID,
                    @"bundleId" : bundleID,
                    @"playing" : @NO,
                    @"playingResolved" : @NO,
                    @"lastPlayingDateError" : @NO,
                    @"elected" : @([playerPath isEqual:electedPath]),
                } mutableCopy];
                MRNowPlayingRequest *request =
                    requestClass
                        ? [[(id)requestClass alloc] initWithPlayerPath:playerPath]
                        : nil;
                SEL isPlayingSelector = NSSelectorFromString(
                    @"requestIsPlayingOnQueue:completion:");
                BOOL supportsScopedPlaying =
                    request && [request respondsToSelector:isPlayingSelector];
                NSDictionary *record = @{
                    @"entry" : entry,
                    @"playerPath" : playerPath,
                    @"supportsScopedPlaying" : @(supportsScopedPlaying),
                };
                [batchRecords addObject:record];

                if (request && supportsScopedPlaying) {
                    dispatch_group_enter(batchGroup);
                    ((void (*)(id, SEL, dispatch_queue_t,
                               void (^)(BOOL, NSError *)))objc_msgSend)(
                        request, isPlayingSelector, requestQueue,
                        ^(BOOL playing, NSError *error) {
                          (void)request;
                          dispatch_async(queue, ^{
                            if (!completed && !error) {
                                entry[@"playing"] = @(playing);
                                entry[@"playingResolved"] = @YES;
                            }
                            dispatch_group_leave(batchGroup);
                          });
                        });
                } else {
                    dispatch_group_enter(batchGroup);
                    getInfoForPlayer(playerPath, NO, queue,
                                     ^(NSDictionary *information) {
                      if (!completed) {
                          copyMetadata(entry, information, stableID, NO, YES);
                      }
                      dispatch_group_leave(batchGroup);
                    });
                }

                if (request) {
                    SEL lastPlayingSelector = NSSelectorFromString(
                        @"requestLastPlayingDateOnQueue:completion:");
                    if ([request respondsToSelector:lastPlayingSelector]) {
                        dispatch_group_enter(batchGroup);
                        ((void (*)(id, SEL, dispatch_queue_t,
                                   void (^)(NSDate *, NSError *)))objc_msgSend)(
                            request, lastPlayingSelector, requestQueue,
                            ^(NSDate *date, NSError *error) {
                              (void)request;
                              dispatch_async(queue, ^{
                                if (!completed) {
                                    if (error) {
                                        entry[@"lastPlayingDateError"] = @YES;
                                    } else {
                                        copyDateSeconds(entry, @"lastPlayingDate",
                                                        date);
                                    }
                                }
                                dispatch_group_leave(batchGroup);
                              });
                            });
                    }
                }

                dispatch_group_leave(batchGroup);
              });
          }

          dispatch_group_notify(batchGroup, queue, ^{
            for (NSDictionary *record in batchRecords) {
                insertTopRecord(topRecords, record,
                                MAX_ENRICHED_PLAYING_CANDIDATES);
            }
            if (processBatch) {
                processBatch(endIndex);
            }
          });
        };
#pragma clang diagnostic pop

        processBatch(0);
    });
}

static void scheduleRefresh(void) {
    dispatch_after(
        dispatch_time(DISPATCH_TIME_NOW, (int64_t)(0.25 * NSEC_PER_SEC)),
        dispatch_get_main_queue(), ^{
          refreshSessions();
        });
}

#if !defined(MEDIA_SESSIONS_SMOKE)
static void printReady(void) {
    printf("{\"ready\":true}\n");
    fflush(stdout);
}
#endif

#if defined(MEDIA_SESSIONS_SMOKE)
@interface SmokeInfiniteDate : NSDate
@end

@implementation SmokeInfiniteDate
- (NSTimeInterval)timeIntervalSinceReferenceDate {
    return INFINITY;
}
- (NSTimeInterval)timeIntervalSince1970 {
    return INFINITY;
}
@end

static NSMutableDictionary *smokeCandidate(NSString *stableID, BOOL playing,
                                           BOOL resolved, NSTimeInterval date,
                                           BOOL hasDate, BOOL elected) {
    NSMutableDictionary *candidate = [@{
        @"stableId" : stableID,
        @"playing" : @(playing),
        @"playingResolved" : @(resolved),
        @"elected" : @(elected),
    } mutableCopy];
    if (hasDate) {
        candidate[@"lastPlayingDate"] = @(date);
    }
    return candidate;
}

static NSDictionary *smokeRecord(NSMutableDictionary *entry) {
    return @{ @"entry" : entry, @"playerPath" : entry };
}

static int runSmokeTests(void) {
    artworkCache = [NSMutableDictionary dictionary];
    artworkCacheBytes = 0;

    if (nextWatchdogToken(0) != 1 ||
        nextWatchdogToken(41) != 42 ||
        nextWatchdogToken(NSUIntegerMax) != 1 ||
        !watchdogTokenIsCurrent(7, 7) ||
        watchdogTokenIsCurrent(0, 0) ||
        watchdogTokenIsCurrent(7, 8)) {
        fprintf(stderr, "watchdog token guard failed\n");
        return 1;
    }

    NSMutableString *longText = [NSMutableString string];
    for (NSUInteger i = 0; i < MAX_TEXT_FIELD_BYTES + 16; i++) {
        [longText appendString:@"a"];
    }
    NSString *bounded = boundedString(longText);
    if ([bounded lengthOfBytesUsingEncoding:NSUTF8StringEncoding] !=
        MAX_TEXT_FIELD_BYTES) {
        fprintf(stderr, "boundedString did not enforce byte cap\n");
        return 1;
    }

    NSString *multibyte = [@"🙂" stringByPaddingToLength:MAX_TEXT_FIELD_BYTES
                                             withString:@"🙂"
                                        startingAtIndex:0];
    bounded = boundedString(multibyte);
    if ([bounded lengthOfBytesUsingEncoding:NSUTF8StringEncoding] >
        MAX_TEXT_FIELD_BYTES) {
        fprintf(stderr, "boundedString returned oversized UTF-8\n");
        return 1;
    }

    NSData *smallArtwork = [@"abcd" dataUsingEncoding:NSUTF8StringEncoding];
    NSString *encoded = cachedArtworkData(@"one", smallArtwork);
    NSUInteger expectedCost = artworkCacheCost(smallArtwork, encoded);
    if (!encoded || artworkCacheBytes != expectedCost || artworkCache.count != 1) {
        fprintf(stderr, "artwork cache insertion accounting failed\n");
        return 1;
    }
    if (cachedArtworkData(@"one", smallArtwork) != encoded ||
        artworkCacheBytes != expectedCost) {
        fprintf(stderr, "artwork cache hit accounting failed\n");
        return 1;
    }

    NSMutableData *largeArtwork =
        [NSMutableData dataWithLength:MAX_RAW_ARTWORK_BYTES - 1];
    NSString *largeEncoded = cachedArtworkData(@"one", largeArtwork);
    NSUInteger largeCost = artworkCacheCost(largeArtwork, largeEncoded);
    if (!largeEncoded || !artworkCache[@"one"] || artworkCacheBytes != largeCost) {
        fprintf(stderr, "artwork cache large insertion accounting failed\n");
        return 1;
    }
    NSString *uncachedEncoded = cachedArtworkData(@"two", largeArtwork);
    if (!uncachedEncoded || artworkCache[@"two"] || artworkCacheBytes != largeCost) {
        fprintf(stderr, "artwork cache oversize replacement accounting failed\n");
        return 1;
    }

    pruneArtworkCache([NSSet set]);
    if (artworkCache.count != 0 || artworkCacheBytes != 0) {
        fprintf(stderr, "artwork cache prune accounting failed\n");
        return 1;
    }

    NSMutableArray *manyCandidates = [NSMutableArray array];
    for (NSUInteger i = 0; i < MAX_ENRICHED_PLAYING_CANDIDATES + 4; i++) {
        [manyCandidates addObject:smokeCandidate(
                                  [NSString stringWithFormat:@"early-%02lu",
                                                             (unsigned long)i],
                                  YES, YES, (NSTimeInterval)i, YES, NO)];
    }
    NSMutableDictionary *lateNewest =
        smokeCandidate(@"late-newest", YES, YES, 5000.0, YES, NO);
    [manyCandidates addObject:lateNewest];
    NSArray *ranked =
        rankedPlayingCandidates(manyCandidates, MAX_ENRICHED_PLAYING_CANDIDATES);
    if (ranked.count != MAX_ENRICHED_PLAYING_CANDIDATES ||
        ranked.firstObject != lateNewest || ![ranked containsObject:lateNewest]) {
        fprintf(stderr, "ranked enrichment dropped a late newest candidate\n");
        return 1;
    }

    NSMutableArray *missingDateTie = [NSMutableArray array];
    for (NSUInteger i = 0; i < MAX_ENRICHED_PLAYING_CANDIDATES + 3; i++) {
        [missingDateTie addObject:smokeCandidate(
                                  [NSString stringWithFormat:@"prefix-%02lu",
                                                             (unsigned long)i],
                                  YES, YES, 0.0, NO, NO)];
    }
    NSMutableDictionary *lateElected =
        smokeCandidate(@"late-elected", YES, YES, 0.0, NO, YES);
    [missingDateTie addObject:lateElected];
    ranked =
        rankedPlayingCandidates(missingDateTie, MAX_ENRICHED_PLAYING_CANDIDATES);
    if (ranked.firstObject != lateElected || ![ranked containsObject:lateElected]) {
        fprintf(stderr, "ranked enrichment dropped a late elected candidate\n");
        return 1;
    }

    NSMutableArray *topRecords = [NSMutableArray array];
    for (NSUInteger i = 0; i < MAX_PHASE1_BATCH_CLIENTS * 3; i++) {
        NSMutableDictionary *entry = smokeCandidate(
            [NSString stringWithFormat:@"batch-%02lu", (unsigned long)i], YES,
            YES, (NSTimeInterval)i, YES, NO);
        insertTopRecord(topRecords, smokeRecord(entry),
                        MAX_ENRICHED_PLAYING_CANDIDATES);
        if (topRecords.count > MAX_ENRICHED_PLAYING_CANDIDATES) {
            fprintf(stderr, "top record set exceeded enrichment cap\n");
            return 1;
        }
    }
    NSMutableDictionary *lateBatchWinner =
        smokeCandidate(@"late-batch-winner", YES, YES, 9000.0, YES, NO);
    insertTopRecord(topRecords, smokeRecord(lateBatchWinner),
                    MAX_ENRICHED_PLAYING_CANDIDATES);
    if ([topRecords.firstObject objectForKey:@"entry"] != lateBatchWinner) {
        fprintf(stderr, "streaming top records dropped late batch winner\n");
        return 1;
    }

    NSArray *publicTop =
        publicCandidates(candidateEntriesFromRecords(topRecords));
    if (publicTop.count != MAX_ENRICHED_PLAYING_CANDIDATES) {
        fprintf(stderr, "public candidate count exceeded top-K cap\n");
        return 1;
    }

    NSMutableDictionary *fallbackEntry =
        smokeCandidate(@"unsupported", NO, NO, 0.0, NO, NO);
    NSData *fallbackArtwork = [@"artwork" dataUsingEncoding:NSUTF8StringEncoding];
    copyMetadata(fallbackEntry,
                 @{
                     @"kMRMediaRemoteNowPlayingInfoPlaybackRate" : @1.0,
                     @"kMRMediaRemoteNowPlayingInfoArtworkData" : fallbackArtwork,
                     @"kMRMediaRemoteNowPlayingInfoTimestamp" :
                         [SmokeInfiniteDate new],
                 },
                 @"unsupported", NO, YES);
    if (![fallbackEntry[@"playing"] boolValue] ||
        ![fallbackEntry[@"playingResolved"] boolValue] ||
        fallbackEntry[@"artworkData"] || fallbackEntry[@"infoUpdateDate"]) {
        fprintf(stderr, "metadata fallback did not stay lightweight and finite\n");
        return 1;
    }

    return 0;
}

int main(int argc, char *argv[]) {
    (void)argc;
    (void)argv;
    @autoreleasepool {
        return runSmokeTests();
    }
}
#else
int main(int argc, char *argv[]) {
    (void)argc;
    helperArgv = argv;
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
#endif
