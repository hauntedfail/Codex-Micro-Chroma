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
static const NSUInteger MAX_PHASE2_BATCH_RECORDS = 4;

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

static BOOL base64EncodedLength(NSUInteger rawLength, NSUInteger *encodedLength) {
    NSUInteger chunks = rawLength / 3;
    NSUInteger remainder = rawLength % 3;
    if (chunks > NSUIntegerMax / 4) {
        return NO;
    }
    NSUInteger length = chunks * 4;
    if (remainder != 0) {
        if (addWouldExceedNSUInteger(length, 4, NSUIntegerMax)) {
            return NO;
        }
        length += 4;
    }
    *encodedLength = length;
    return YES;
}

static BOOL artworkCacheCostForLengths(NSUInteger rawLength,
                                       NSUInteger encodedLength,
                                       NSUInteger *cost) {
    if (addWouldExceedNSUInteger(rawLength, encodedLength, NSUIntegerMax)) {
        return NO;
    }
    *cost = rawLength + encodedLength;
    return YES;
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

    removeCachedArtwork(stableID);
    NSUInteger encodedLength = 0;
    NSUInteger cost = 0;
    if (!base64EncodedLength(artwork.length, &encodedLength) ||
        !artworkCacheCostForLengths(artwork.length, encodedLength, &cost) ||
        addWouldExceedNSUInteger(artworkCacheBytes, cost,
                                 MAX_ARTWORK_CACHE_BYTES)) {
        return nil;
    }

    NSString *encoded = [artwork base64EncodedStringWithOptions:0];
    if ([encoded lengthOfBytesUsingEncoding:NSUTF8StringEncoding] !=
        encodedLength) {
        return nil;
    }
    cost = artworkCacheCost(artwork, encoded);
    if (addWouldExceedNSUInteger(artworkCacheBytes, cost,
                                 MAX_ARTWORK_CACHE_BYTES)) {
        return nil;
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

static NSData *recordArtworkData(NSDictionary *record) {
    NSDictionary *information = record[@"information"];
    if (![information isKindOfClass:[NSDictionary class]]) {
        return nil;
    }
    id artwork = information[@"kMRMediaRemoteNowPlayingInfoArtworkData"];
    return [artwork isKindOfClass:[NSData class]] ? artwork : nil;
}

static void copyString(NSMutableDictionary *destination, NSString *outputKey,
                       NSDictionary *source, NSString *sourceKey);
static void copyNumber(NSMutableDictionary *destination, NSString *outputKey,
                       NSDictionary *source, NSString *sourceKey);

static NSDictionary *sanitizedInformationForRetention(NSDictionary *information) {
    if (![information isKindOfClass:[NSDictionary class]]) {
        return nil;
    }

    NSMutableDictionary *sanitized = [NSMutableDictionary dictionary];
    copyString(sanitized, @"kMRMediaRemoteNowPlayingInfoTitle", information,
               @"kMRMediaRemoteNowPlayingInfoTitle");
    copyString(sanitized, @"kMRMediaRemoteNowPlayingInfoArtist", information,
               @"kMRMediaRemoteNowPlayingInfoArtist");
    copyString(sanitized, @"kMRMediaRemoteNowPlayingInfoAlbum", information,
               @"kMRMediaRemoteNowPlayingInfoAlbum");
    copyNumber(sanitized, @"kMRMediaRemoteNowPlayingInfoElapsedTime",
               information, @"kMRMediaRemoteNowPlayingInfoElapsedTime");
    copyNumber(sanitized, @"kMRMediaRemoteNowPlayingInfoDuration",
               information, @"kMRMediaRemoteNowPlayingInfoDuration");
    copyNumber(sanitized, @"kMRMediaRemoteNowPlayingInfoPlaybackRate",
               information, @"kMRMediaRemoteNowPlayingInfoPlaybackRate");
    NSDate *timestamp = information[@"kMRMediaRemoteNowPlayingInfoTimestamp"];
    if ([timestamp isKindOfClass:[NSDate class]] &&
        isfinite([timestamp timeIntervalSince1970])) {
        sanitized[@"kMRMediaRemoteNowPlayingInfoTimestamp"] = timestamp;
    }
    id artwork = information[@"kMRMediaRemoteNowPlayingInfoArtworkData"];
    if ([artwork isKindOfClass:[NSData class]] &&
        [(NSData *)artwork length] <= MAX_RAW_ARTWORK_BYTES) {
        sanitized[@"kMRMediaRemoteNowPlayingInfoArtworkData"] = artwork;
    }

    return sanitized;
}

static NSUInteger retainedRawArtworkBytesForRecords(NSArray *records) {
    NSUInteger total = 0;
    for (NSDictionary *record in records) {
        NSData *artworkData = recordArtworkData(record);
        if (!artworkData) {
            continue;
        }
        if (addWouldExceedNSUInteger(total, artworkData.length, NSUIntegerMax)) {
            return NSUIntegerMax;
        }
        total += artworkData.length;
    }
    return total;
}

typedef struct {
    NSMutableSet<NSString *> *selectedStableIDs;
    NSMutableSet<NSString *> *seenStableIDs;
    NSUInteger serializedArtworkBytes;
    NSUInteger selectedCacheBytes;
} ArtworkSelectionState;

static ArtworkSelectionState makeArtworkSelectionState(void) {
    ArtworkSelectionState state;
    state.selectedStableIDs = [NSMutableSet set];
    state.seenStableIDs = [NSMutableSet set];
    state.serializedArtworkBytes = 0;
    state.selectedCacheBytes = 0;
    return state;
}

static void evictUnselectedArtworkUntilAvailable(NSSet<NSString *> *stableIDs,
                                                 NSUInteger requiredBytes) {
    if (!addWouldExceedNSUInteger(artworkCacheBytes, requiredBytes,
                                  MAX_ARTWORK_CACHE_BYTES)) {
        return;
    }

    NSArray *keys = [[artworkCache allKeys]
        sortedArrayUsingSelector:@selector(compare:)];
    for (NSString *stableID in keys) {
        if ([stableIDs containsObject:stableID]) {
            continue;
        }
        removeCachedArtwork(stableID);
        if (!addWouldExceedNSUInteger(artworkCacheBytes, requiredBytes,
                                      MAX_ARTWORK_CACHE_BYTES)) {
            return;
        }
    }
}

static void prepareArtworkCacheForSelectedRecords(NSArray *records,
                                                  NSSet<NSString *> *stableIDs) {
    NSUInteger requiredBytes = 0;
    for (NSDictionary *record in records) {
        if (![record[@"includeArtwork"] boolValue]) {
            continue;
        }
        NSDictionary *entry = record[@"entry"];
        NSString *stableID = entry[@"stableId"];
        NSData *artworkData = recordArtworkData(record);
        if (![stableID isKindOfClass:[NSString class]] || !artworkData) {
            continue;
        }

        NSDictionary *cached = artworkCache[stableID];
        NSData *cachedData = cached[@"data"];
        NSString *cachedEncoded = cached[@"encoded"];
        if ([cachedData isKindOfClass:[NSData class]] &&
            ![cachedData isEqualToData:artworkData]) {
            removeCachedArtwork(stableID);
            cached = nil;
            cachedData = nil;
            cachedEncoded = nil;
        }
        if ([cachedData isKindOfClass:[NSData class]] &&
            [cachedEncoded isKindOfClass:[NSString class]] &&
            [cachedData isEqualToData:artworkData]) {
            continue;
        }

        NSUInteger encodedLength = 0;
        NSUInteger cost = 0;
        if (base64EncodedLength(artworkData.length, &encodedLength) &&
            artworkCacheCostForLengths(artworkData.length, encodedLength,
                                       &cost)) {
            if (addWouldExceedNSUInteger(requiredBytes, cost,
                                         NSUIntegerMax)) {
                requiredBytes = NSUIntegerMax;
            } else {
                requiredBytes += cost;
            }
        }
    }

    evictUnselectedArtworkUntilAvailable(stableIDs, requiredBytes);
}

static void markSelectedArtworkForRecordsWithState(NSArray *records,
                                                   ArtworkSelectionState *state) {
    for (NSMutableDictionary *record in records) {
        if (![record isKindOfClass:[NSMutableDictionary class]]) {
            continue;
        }
        [record removeObjectForKey:@"includeArtwork"];
        NSDictionary *entry = record[@"entry"];
        NSString *stableID = entry[@"stableId"];
        if (![entry isKindOfClass:[NSDictionary class]] ||
            ![stableID isKindOfClass:[NSString class]] ||
            [state->seenStableIDs containsObject:stableID] ||
            ![entry[@"playing"] boolValue] ||
            ![entry[@"playingResolved"] boolValue]) {
            continue;
        }
        [state->seenStableIDs addObject:stableID];

        NSData *artworkData = recordArtworkData(record);
        if (!artworkData) {
            continue;
        }
        if (artworkData.length > MAX_RAW_ARTWORK_BYTES) {
            continue;
        }

        NSUInteger encodedLength = 0;
        NSUInteger cost = 0;
        if (!base64EncodedLength(artworkData.length, &encodedLength) ||
            encodedLength > MAX_SERIALIZED_ARTWORK_BYTES ||
            addWouldExceedNSUInteger(state->serializedArtworkBytes, encodedLength,
                                     MAX_SERIALIZED_ARTWORK_BYTES) ||
            !artworkCacheCostForLengths(artworkData.length, encodedLength,
                                        &cost) ||
            addWouldExceedNSUInteger(state->selectedCacheBytes, cost,
                                     MAX_ARTWORK_CACHE_BYTES)) {
            continue;
        }

        [state->selectedStableIDs addObject:stableID];
        record[@"includeArtwork"] = @YES;
        state->serializedArtworkBytes += encodedLength;
        state->selectedCacheBytes += cost;
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
          NSArray *rankedRecords = [topRecords copy];
          __block ArtworkSelectionState selectionState =
              makeArtworkSelectionState();
          __block void (^processEnrichmentBatch)(NSUInteger) = nil;

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Warc-retain-cycles"
          processEnrichmentBatch = ^(NSUInteger startIndex) {
            if (completed) {
                processEnrichmentBatch = nil;
                return;
            }
            if (startIndex >= rankedRecords.count) {
                pruneArtworkCache(selectionState.selectedStableIDs);
                complete(NO);
                processEnrichmentBatch = nil;
                return;
            }

            armWatchdog();
            NSUInteger endIndex = MIN(rankedRecords.count,
                                      startIndex + MAX_PHASE2_BATCH_RECORDS);
            NSArray *batchRecords =
                [rankedRecords subarrayWithRange:NSMakeRange(
                                   startIndex, endIndex - startIndex)];
            dispatch_group_t enrichmentGroup = dispatch_group_create();

            for (NSDictionary *record in batchRecords) {
                NSMutableDictionary *entry = record[@"entry"];
                NSString *stableID = entry[@"stableId"];
                id playerPath = record[@"playerPath"];
                if (!playerPath ||
                    ![stableID isKindOfClass:[NSString class]]) {
                    continue;
                }
                dispatch_group_enter(enrichmentGroup);
                getInfoForPlayer(playerPath, YES, queue,
                                 ^(NSDictionary *information) {
                  if (!completed && [record isKindOfClass:[NSMutableDictionary class]]) {
                      NSDictionary *sanitized =
                          sanitizedInformationForRetention(information);
                      if (sanitized) {
                          ((NSMutableDictionary *)record)[@"information"] =
                              sanitized;
                      }
                  }
                  dispatch_group_leave(enrichmentGroup);
                });
            }

            dispatch_group_notify(enrichmentGroup, queue, ^{
              if (completed) {
                  processEnrichmentBatch = nil;
                  return;
              }
              if (retainedRawArtworkBytesForRecords(batchRecords) >
                  MAX_ARTWORK_CACHE_BYTES) {
                  fprintf(stderr,
                          "phase-2 retained artwork exceeded batch cap\n");
                  restartAfterTimeout();
              }
              markSelectedArtworkForRecordsWithState(batchRecords,
                                                     &selectionState);
              prepareArtworkCacheForSelectedRecords(batchRecords,
                                                    selectionState.selectedStableIDs);
              for (NSDictionary *record in batchRecords) {
                  NSMutableDictionary *entry = record[@"entry"];
                  NSDictionary *information = record[@"information"];
                  NSString *stableID = entry[@"stableId"];
                  if (![entry isKindOfClass:[NSMutableDictionary class]] ||
                      ![information isKindOfClass:[NSDictionary class]] ||
                      ![stableID isKindOfClass:[NSString class]]) {
                      continue;
                  }
                  copyMetadata(entry, information, stableID,
                               [record[@"includeArtwork"] boolValue],
                               NO);
                  if ([record isKindOfClass:[NSMutableDictionary class]]) {
                      [(NSMutableDictionary *)record removeObjectForKey:@"includeArtwork"];
                      [(NSMutableDictionary *)record removeObjectForKey:@"information"];
                  }
              }
              processEnrichmentBatch(endIndex);
            });
          };
#pragma clang diagnostic pop

          processEnrichmentBatch(0);
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
                NSMutableDictionary *record = [@{
                    @"entry" : entry,
                    @"playerPath" : playerPath,
                    @"supportsScopedPlaying" : @(supportsScopedPlaying),
                } mutableCopy];
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

static NSMutableDictionary *smokeRecord(NSMutableDictionary *entry) {
    return [@{ @"entry" : entry, @"playerPath" : entry } mutableCopy];
}

static NSSet<NSString *> *markSelectedArtworkForRecords(NSArray *records) {
    ArtworkSelectionState state = makeArtworkSelectionState();
    markSelectedArtworkForRecordsWithState(records, &state);
    return state.selectedStableIDs;
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
    if (uncachedEncoded || artworkCache[@"two"] || artworkCacheBytes != largeCost) {
        fprintf(stderr, "artwork cache oversize replacement accounting failed\n");
        return 1;
    }

    pruneArtworkCache([NSSet set]);
    if (artworkCache.count != 0 || artworkCacheBytes != 0) {
        fprintf(stderr, "artwork cache prune accounting failed\n");
        return 1;
    }

    NSUInteger encodedLength = 0;
    if (!base64EncodedLength(0, &encodedLength) || encodedLength != 0 ||
        !base64EncodedLength(1, &encodedLength) || encodedLength != 4 ||
        !base64EncodedLength(2, &encodedLength) || encodedLength != 4 ||
        !base64EncodedLength(3, &encodedLength) || encodedLength != 4 ||
        !base64EncodedLength(4, &encodedLength) || encodedLength != 8) {
        fprintf(stderr, "base64 encoded length accounting failed\n");
        return 1;
    }

    NSMutableData *maxRetainedArtwork =
        [NSMutableData dataWithLength:MAX_RAW_ARTWORK_BYTES];
    NSMutableData *oversizedRetainedArtwork =
        [NSMutableData dataWithLength:MAX_RAW_ARTWORK_BYTES + 1];
    NSDictionary *keptInformation = sanitizedInformationForRetention(@{
        @"kMRMediaRemoteNowPlayingInfoArtworkData" : maxRetainedArtwork,
        @"kMRMediaRemoteNowPlayingInfoTitle" : @"kept",
    });
    NSDictionary *strippedInformation = sanitizedInformationForRetention(@{
        @"kMRMediaRemoteNowPlayingInfoArtworkData" : oversizedRetainedArtwork,
        @"kMRMediaRemoteNowPlayingInfoTitle" : @"stripped",
    });
    NSDictionary *nonDataInformation = sanitizedInformationForRetention(@{
        @"kMRMediaRemoteNowPlayingInfoArtworkData" : @"not-data",
        @"kMRMediaRemoteNowPlayingInfoTitle" : @"non-data",
    });
    NSMutableString *oversizedTitle = [NSMutableString string];
    for (NSUInteger i = 0; i < MAX_TEXT_FIELD_BYTES + 64; i++) {
        [oversizedTitle appendString:@"t"];
    }
    NSMutableData *irrelevantLargeObject =
        [NSMutableData dataWithLength:MAX_RAW_ARTWORK_BYTES + 4096];
    NSDate *finiteTimestamp = [NSDate dateWithTimeIntervalSince1970:1234.0];
    NSDictionary *boundedKnownInformation = sanitizedInformationForRetention(@{
        @"kMRMediaRemoteNowPlayingInfoTitle" : oversizedTitle,
        @"kMRMediaRemoteNowPlayingInfoArtist" : @"artist",
        @"kMRMediaRemoteNowPlayingInfoAlbum" : @42,
        @"kMRMediaRemoteNowPlayingInfoElapsedTime" : @12.5,
        @"kMRMediaRemoteNowPlayingInfoDuration" : @(INFINITY),
        @"kMRMediaRemoteNowPlayingInfoPlaybackRate" : @1.0,
        @"kMRMediaRemoteNowPlayingInfoTimestamp" : finiteTimestamp,
        @"kMRMediaRemoteNowPlayingInfoArtworkData" : maxRetainedArtwork,
        @"irrelevantLargeObject" : irrelevantLargeObject,
    });
    NSDictionary *invalidKnownInformation = sanitizedInformationForRetention(@{
        @"kMRMediaRemoteNowPlayingInfoElapsedTime" : @(NAN),
        @"kMRMediaRemoteNowPlayingInfoDuration" : @(INFINITY),
        @"kMRMediaRemoteNowPlayingInfoPlaybackRate" : @(-INFINITY),
        @"kMRMediaRemoteNowPlayingInfoTimestamp" : [SmokeInfiniteDate new],
        @"irrelevantLargeObject" : irrelevantLargeObject,
    });
    NSDictionary *emptyKnownInformation = sanitizedInformationForRetention(@{
        @"irrelevantLargeObject" : irrelevantLargeObject,
    });
    if (keptInformation == nil ||
        keptInformation[@"kMRMediaRemoteNowPlayingInfoArtworkData"] !=
            maxRetainedArtwork ||
        strippedInformation[@"kMRMediaRemoteNowPlayingInfoArtworkData"] ||
        nonDataInformation[@"kMRMediaRemoteNowPlayingInfoArtworkData"] ||
        ![strippedInformation[@"kMRMediaRemoteNowPlayingInfoTitle"]
            isEqualToString:@"stripped"]) {
        fprintf(stderr, "retained information artwork sanitization failed\n");
        return 1;
    }
    if ([boundedKnownInformation[@"kMRMediaRemoteNowPlayingInfoTitle"]
            lengthOfBytesUsingEncoding:NSUTF8StringEncoding] !=
            MAX_TEXT_FIELD_BYTES ||
        ![boundedKnownInformation[@"kMRMediaRemoteNowPlayingInfoArtist"]
            isEqualToString:@"artist"] ||
        boundedKnownInformation[@"kMRMediaRemoteNowPlayingInfoAlbum"] ||
        ![boundedKnownInformation[@"kMRMediaRemoteNowPlayingInfoElapsedTime"]
            isEqual:@12.5] ||
        boundedKnownInformation[@"kMRMediaRemoteNowPlayingInfoDuration"] ||
        ![boundedKnownInformation[@"kMRMediaRemoteNowPlayingInfoPlaybackRate"]
            isEqual:@1.0] ||
        boundedKnownInformation[@"kMRMediaRemoteNowPlayingInfoTimestamp"] !=
            finiteTimestamp ||
        boundedKnownInformation[@"kMRMediaRemoteNowPlayingInfoArtworkData"] !=
            maxRetainedArtwork ||
        boundedKnownInformation[@"irrelevantLargeObject"]) {
        fprintf(stderr, "retained information whitelist/bounds failed\n");
        return 1;
    }
    if (invalidKnownInformation.count != 0 ||
        emptyKnownInformation.count != 0) {
        fprintf(stderr, "retained information invalid value stripping failed\n");
        return 1;
    }

    NSMutableArray *phase2BatchRecords = [NSMutableArray array];
    for (NSUInteger i = 0; i < MAX_PHASE2_BATCH_RECORDS; i++) {
        NSMutableDictionary *entry = smokeCandidate(
            [NSString stringWithFormat:@"phase2-%lu", (unsigned long)i],
            YES, YES, (NSTimeInterval)i, YES, NO);
        NSMutableDictionary *record = smokeRecord(entry);
        record[@"information"] = @{
            @"kMRMediaRemoteNowPlayingInfoArtworkData" : maxRetainedArtwork,
        };
        [phase2BatchRecords addObject:record];
    }
    if (MAX_PHASE2_BATCH_RECORDS != 4 ||
        retainedRawArtworkBytesForRecords(phase2BatchRecords) !=
            MAX_ARTWORK_CACHE_BYTES) {
        fprintf(stderr, "phase-2 retained artwork batch bound failed\n");
        return 1;
    }

    NSMutableData *winnerArtwork =
        [NSMutableData dataWithLength:MAX_RAW_ARTWORK_BYTES - 1];
    NSMutableData *rejectedArtwork =
        [NSMutableData dataWithLength:MAX_RAW_ARTWORK_BYTES - 1];
    NSMutableData *thirdArtwork = [NSMutableData dataWithLength:3];
    NSMutableDictionary *winner =
        smokeCandidate(@"winner", YES, YES, 3000.0, YES, NO);
    NSMutableDictionary *runnerUp =
        smokeCandidate(@"runner-up", YES, YES, 2000.0, YES, NO);
    NSMutableDictionary *third =
        smokeCandidate(@"third", YES, YES, 1000.0, YES, NO);
    NSMutableDictionary *winnerRecord = smokeRecord(winner);
    NSMutableDictionary *runnerUpRecord = smokeRecord(runnerUp);
    NSMutableDictionary *thirdRecord = smokeRecord(third);
    winnerRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" : winnerArtwork };
    runnerUpRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" : rejectedArtwork };
    thirdRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" : thirdArtwork };
    NSArray *selectionRecords = @[ winnerRecord, runnerUpRecord, thirdRecord ];
    NSSet<NSString *> *selectedArtworkStableIDs =
        markSelectedArtworkForRecords(selectionRecords);
    if (![selectedArtworkStableIDs containsObject:@"winner"] ||
        [selectedArtworkStableIDs containsObject:@"runner-up"] ||
        ![selectedArtworkStableIDs containsObject:@"third"] ||
        ![winnerRecord[@"includeArtwork"] boolValue] ||
        runnerUpRecord[@"includeArtwork"] ||
        ![thirdRecord[@"includeArtwork"] boolValue]) {
        fprintf(stderr, "artwork selection did not preserve winner-first budget\n");
        return 1;
    }
    prepareArtworkCacheForSelectedRecords(selectionRecords,
                                          selectedArtworkStableIDs);
    for (NSMutableDictionary *record in selectionRecords) {
        NSMutableDictionary *entry = record[@"entry"];
        NSString *stableID = entry[@"stableId"];
        copyMetadata(entry, record[@"information"], stableID,
                     [record[@"includeArtwork"] boolValue], NO);
        [record removeObjectForKey:@"includeArtwork"];
        [record removeObjectForKey:@"information"];
    }
    if (!winner[@"artworkData"] || runnerUp[@"artworkData"] ||
        !third[@"artworkData"] || artworkCache.count != 2 ||
        !artworkCache[@"winner"] ||
        !artworkCache[@"third"] ||
        artworkCacheBytes !=
            artworkCacheCost(winnerArtwork, winner[@"artworkData"]) +
                artworkCacheCost(thirdArtwork, third[@"artworkData"])) {
        fprintf(stderr, "artwork selection/cache admission failed\n");
        return 1;
    }

    NSMutableDictionary *batchedWinner =
        smokeCandidate(@"batched-winner", YES, YES, 3000.0, YES, NO);
    NSMutableDictionary *batchedRunnerUp =
        smokeCandidate(@"batched-runner-up", YES, YES, 2000.0, YES, NO);
    NSMutableDictionary *batchedThird =
        smokeCandidate(@"batched-third", YES, YES, 1000.0, YES, NO);
    NSMutableDictionary *batchedWinnerRecord = smokeRecord(batchedWinner);
    NSMutableDictionary *batchedRunnerUpRecord = smokeRecord(batchedRunnerUp);
    NSMutableDictionary *batchedThirdRecord = smokeRecord(batchedThird);
    batchedWinnerRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" : winnerArtwork };
    batchedRunnerUpRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" : rejectedArtwork };
    batchedThirdRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" : thirdArtwork };
    ArtworkSelectionState batchedState = makeArtworkSelectionState();
    markSelectedArtworkForRecordsWithState(@[ batchedWinnerRecord,
                                              batchedRunnerUpRecord ],
                                           &batchedState);
    markSelectedArtworkForRecordsWithState(@[ batchedThirdRecord ],
                                           &batchedState);
    if (![batchedState.selectedStableIDs containsObject:@"batched-winner"] ||
        [batchedState.selectedStableIDs containsObject:@"batched-runner-up"] ||
        ![batchedState.selectedStableIDs containsObject:@"batched-third"] ||
        ![batchedWinnerRecord[@"includeArtwork"] boolValue] ||
        batchedRunnerUpRecord[@"includeArtwork"] ||
        ![batchedThirdRecord[@"includeArtwork"] boolValue]) {
        fprintf(stderr, "batched artwork selection lost cumulative budget\n");
        return 1;
    }

    artworkCache = [NSMutableDictionary dictionary];
    artworkCacheBytes = 0;
    NSData *futureArtwork = [@"future-artwork" dataUsingEncoding:NSUTF8StringEncoding];
    NSString *futureEncoded = cachedArtworkData(@"future", futureArtwork);
    if (!futureEncoded) {
        fprintf(stderr, "future cache setup failed\n");
        return 1;
    }
    NSMutableDictionary *firstBatchEntry =
        smokeCandidate(@"first-batch", YES, YES, 5000.0, YES, NO);
    NSMutableDictionary *futureEntry =
        smokeCandidate(@"future", YES, YES, 4000.0, YES, NO);
    NSMutableDictionary *firstBatchRecord = smokeRecord(firstBatchEntry);
    NSMutableDictionary *futureRecord = smokeRecord(futureEntry);
    firstBatchRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" :
               [@"first-batch-artwork" dataUsingEncoding:NSUTF8StringEncoding] };
    futureRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" : futureArtwork };
    ArtworkSelectionState futureState = makeArtworkSelectionState();
    markSelectedArtworkForRecordsWithState(@[ firstBatchRecord ], &futureState);
    prepareArtworkCacheForSelectedRecords(@[ firstBatchRecord ],
                                          futureState.selectedStableIDs);
    copyMetadata(firstBatchEntry, firstBatchRecord[@"information"], @"first-batch",
                 [firstBatchRecord[@"includeArtwork"] boolValue], NO);
    if (artworkCache[@"future"] == nil ||
        cachedArtworkData(@"future", futureArtwork) != futureEncoded) {
        fprintf(stderr, "future batch exact cache hit was pruned early\n");
        return 1;
    }
    markSelectedArtworkForRecordsWithState(@[ futureRecord ], &futureState);
    prepareArtworkCacheForSelectedRecords(@[ futureRecord ],
                                          futureState.selectedStableIDs);
    copyMetadata(futureEntry, futureRecord[@"information"], @"future",
                 [futureRecord[@"includeArtwork"] boolValue], NO);
    if (futureEntry[@"artworkData"] != futureEncoded ||
        artworkCache[@"future"][@"encoded"] != futureEncoded) {
        fprintf(stderr, "future batch exact cache hit was re-encoded\n");
        return 1;
    }
    pruneArtworkCache(futureState.selectedStableIDs);
    if (!artworkCache[@"first-batch"] || !artworkCache[@"future"]) {
        fprintf(stderr, "final selected cache prune removed selected entries\n");
        return 1;
    }

    artworkCache = [NSMutableDictionary dictionary];
    artworkCacheBytes = 0;
    NSMutableData *oldLowerArtwork =
        [NSMutableData dataWithLength:MAX_RAW_ARTWORK_BYTES - 1];
    NSMutableData *newWinnerArtwork =
        [NSMutableData dataWithLength:MAX_RAW_ARTWORK_BYTES - 1];
    NSMutableData *newLowerArtwork = [NSMutableData dataWithLength:3];
    if (!cachedArtworkData(@"lower", oldLowerArtwork)) {
        fprintf(stderr, "stale artwork cache setup failed\n");
        return 1;
    }
    NSMutableDictionary *changedWinner =
        smokeCandidate(@"changed-winner", YES, YES, 4000.0, YES, NO);
    NSMutableDictionary *changedLower =
        smokeCandidate(@"lower", YES, YES, 3000.0, YES, NO);
    NSMutableDictionary *changedWinnerRecord = smokeRecord(changedWinner);
    NSMutableDictionary *changedLowerRecord = smokeRecord(changedLower);
    changedWinnerRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" : newWinnerArtwork };
    changedLowerRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" : newLowerArtwork };
    NSArray *changedRecords = @[ changedWinnerRecord, changedLowerRecord ];
    selectedArtworkStableIDs = markSelectedArtworkForRecords(changedRecords);
    prepareArtworkCacheForSelectedRecords(changedRecords,
                                          selectedArtworkStableIDs);
    if (artworkCache[@"lower"]) {
        fprintf(stderr, "stale selected cache entry was not evicted\n");
        return 1;
    }
    for (NSMutableDictionary *record in changedRecords) {
        NSMutableDictionary *entry = record[@"entry"];
        NSString *stableID = entry[@"stableId"];
        copyMetadata(entry, record[@"information"], stableID,
                     [record[@"includeArtwork"] boolValue], NO);
    }
    if (!changedWinner[@"artworkData"] || !changedLower[@"artworkData"] ||
        !artworkCache[@"changed-winner"] || !artworkCache[@"lower"]) {
        fprintf(stderr, "stale selected cache entry blocked changed artwork\n");
        return 1;
    }

    artworkCache = [NSMutableDictionary dictionary];
    artworkCacheBytes = 0;
    NSMutableDictionary *duplicateFirst =
        smokeCandidate(@"duplicate", YES, YES, 2000.0, YES, NO);
    NSMutableDictionary *duplicateSecond =
        smokeCandidate(@"duplicate", YES, YES, 1000.0, YES, NO);
    NSMutableDictionary *duplicateFirstRecord = smokeRecord(duplicateFirst);
    NSMutableDictionary *duplicateSecondRecord = smokeRecord(duplicateSecond);
    duplicateFirstRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" :
               [@"first" dataUsingEncoding:NSUTF8StringEncoding] };
    duplicateSecondRecord[@"information"] =
        @{ @"kMRMediaRemoteNowPlayingInfoArtworkData" :
               [@"second" dataUsingEncoding:NSUTF8StringEncoding] };
    NSArray *duplicateRecords =
        @[ duplicateFirstRecord, duplicateSecondRecord ];
    selectedArtworkStableIDs = markSelectedArtworkForRecords(duplicateRecords);
    if (selectedArtworkStableIDs.count != 1 ||
        ![duplicateFirstRecord[@"includeArtwork"] boolValue] ||
        duplicateSecondRecord[@"includeArtwork"]) {
        fprintf(stderr, "duplicate stable ID artwork selection was not record-specific\n");
        return 1;
    }
    prepareArtworkCacheForSelectedRecords(duplicateRecords,
                                          selectedArtworkStableIDs);
    for (NSMutableDictionary *record in duplicateRecords) {
        NSMutableDictionary *entry = record[@"entry"];
        NSString *stableID = entry[@"stableId"];
        copyMetadata(entry, record[@"information"], stableID,
                     [record[@"includeArtwork"] boolValue], NO);
    }
    if (!duplicateFirst[@"artworkData"] || duplicateSecond[@"artworkData"] ||
        artworkCache.count != 1) {
        fprintf(stderr, "duplicate stable ID encoded more artwork than budgeted\n");
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
