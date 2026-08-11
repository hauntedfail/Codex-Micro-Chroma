#import "process_tap.h"

#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#import <CoreAudio/CoreAudio.h>
#import <Foundation/Foundation.h>

static const uint32_t kChromaMaximumFrames = 4096;

static NSString *ChromaOSStatusMessage(NSString *operation, OSStatus status) {
    uint32_t value = CFSwapInt32HostToBig((uint32_t)status);
    char code[5] = {0};
    memcpy(code, &value, 4);
    BOOL printable = YES;
    for (NSUInteger index = 0; index < 4; index++) {
        if (code[index] < 32 || code[index] > 126) {
            printable = NO;
            break;
        }
    }
    if (printable) {
        return [NSString stringWithFormat:@"%@ failed with OSStatus %d ('%s')", operation, status, code];
    }
    return [NSString stringWithFormat:@"%@ failed with OSStatus %d", operation, status];
}

static OSStatus ChromaReadProperty(
    AudioObjectID objectID,
    AudioObjectPropertySelector selector,
    uint32_t *size,
    void *output
) {
    AudioObjectPropertyAddress address = {
        .mSelector = selector,
        .mScope = kAudioObjectPropertyScopeGlobal,
        .mElement = kAudioObjectPropertyElementMain,
    };
    return AudioObjectGetPropertyData(objectID, &address, 0, NULL, size, output);
}

@interface ChromaProcessTap : NSObject {
    AudioObjectID _tapID;
    AudioObjectID _aggregateDeviceID;
    AudioDeviceIOProcID _ioProcID;
    ChromaAudioCallback _callback;
    void *_context;
    AudioStreamBasicDescription _format;
    float *_leftScratch;
    float *_rightScratch;
}

- (BOOL)startWithCallback:(ChromaAudioCallback)callback
                  context:(void *)context
                    error:(NSString **)error;
- (void)invalidate;
@property(nonatomic, readonly) double sampleRate;
@end

@implementation ChromaProcessTap

- (instancetype)init {
    self = [super init];
    if (self) {
        _tapID = kAudioObjectUnknown;
        _aggregateDeviceID = kAudioObjectUnknown;
        _ioProcID = NULL;
        _leftScratch = calloc(kChromaMaximumFrames, sizeof(float));
        _rightScratch = calloc(kChromaMaximumFrames, sizeof(float));
        if (_leftScratch == NULL || _rightScratch == NULL) {
            free(_leftScratch);
            free(_rightScratch);
            return nil;
        }
    }
    return self;
}

- (void)dealloc {
    [self invalidate];
    free(_leftScratch);
    free(_rightScratch);
}

- (double)sampleRate {
    return _format.mSampleRate;
}

- (BOOL)startWithCallback:(ChromaAudioCallback)callback
                  context:(void *)context
                    error:(NSString **)error {
    _callback = callback;
    _context = context;

    CATapDescription *description = [[CATapDescription alloc]
        initStereoGlobalTapButExcludeProcesses:@[]];
    [description setName:@"Codex Micro Chroma System Audio"];
    [description setPrivate:YES];
    [description setMuteBehavior:CATapUnmuted];

    OSStatus status = AudioHardwareCreateProcessTap(description, &_tapID);
    if (status != noErr) {
        if (error) *error = ChromaOSStatusMessage(@"AudioHardwareCreateProcessTap", status);
        [self invalidate];
        return NO;
    }
    if (_tapID == kAudioObjectUnknown) {
        if (error) *error = @"AudioHardwareCreateProcessTap returned an unknown tap ID";
        [self invalidate];
        return NO;
    }

    uint32_t formatSize = sizeof(_format);
    status = ChromaReadProperty(_tapID, kAudioTapPropertyFormat, &formatSize, &_format);
    if (status != noErr) {
        if (error) {
            *error = [NSString stringWithFormat:@"%@ (tap ID %u)",
                ChromaOSStatusMessage(@"reading tap format", status), _tapID];
        }
        [self invalidate];
        return NO;
    }
    BOOL floatPCM = _format.mFormatID == kAudioFormatLinearPCM
        && (_format.mFormatFlags & kAudioFormatFlagIsFloat) != 0
        && _format.mBitsPerChannel == 32;
    if (!floatPCM || _format.mChannelsPerFrame == 0) {
        if (error) {
            *error = [NSString stringWithFormat:
                @"unsupported tap format: id=%u flags=%u channels=%u bits=%u",
                _format.mFormatID,
                _format.mFormatFlags,
                _format.mChannelsPerFrame,
                _format.mBitsPerChannel];
        }
        [self invalidate];
        return NO;
    }

    AudioObjectID outputDeviceID = kAudioObjectUnknown;
    uint32_t outputDeviceSize = sizeof(outputDeviceID);
    status = ChromaReadProperty(
        kAudioObjectSystemObject,
        kAudioHardwarePropertyDefaultSystemOutputDevice,
        &outputDeviceSize,
        &outputDeviceID
    );
    if (status != noErr) {
        if (error) *error = ChromaOSStatusMessage(@"reading default system output", status);
        [self invalidate];
        return NO;
    }

    CFStringRef outputUIDRef = NULL;
    uint32_t outputUIDSize = sizeof(outputUIDRef);
    status = ChromaReadProperty(
        outputDeviceID,
        kAudioDevicePropertyDeviceUID,
        &outputUIDSize,
        &outputUIDRef
    );
    if (status != noErr || outputUIDRef == NULL) {
        if (error) *error = ChromaOSStatusMessage(@"reading output device UID", status);
        [self invalidate];
        return NO;
    }
    NSString *outputUID = CFBridgingRelease(outputUIDRef);

    // The real output device anchors the aggregate clock. A tap-only aggregate can block in
    // AudioDeviceStart on current macOS releases even though creation succeeds.
    NSDictionary *aggregateDescription = @{
        @kAudioAggregateDeviceNameKey: @"Codex Micro Chroma Process Tap",
        @kAudioAggregateDeviceUIDKey: [NSUUID UUID].UUIDString,
        @kAudioAggregateDeviceMainSubDeviceKey: outputUID,
        @kAudioAggregateDeviceIsPrivateKey: @YES,
        @kAudioAggregateDeviceIsStackedKey: @NO,
        @kAudioAggregateDeviceTapAutoStartKey: @YES,
        @kAudioAggregateDeviceSubDeviceListKey: @[
            @{ @kAudioSubDeviceUIDKey: outputUID }
        ],
        @kAudioAggregateDeviceTapListKey: @[
            @{
                @kAudioSubTapDriftCompensationKey: @YES,
                @kAudioSubTapUIDKey: description.UUID.UUIDString,
            }
        ],
    };

    status = AudioHardwareCreateAggregateDevice(
        (__bridge CFDictionaryRef)aggregateDescription,
        &_aggregateDeviceID
    );
    if (status != noErr) {
        if (error) *error = ChromaOSStatusMessage(@"AudioHardwareCreateAggregateDevice", status);
        [self invalidate];
        return NO;
    }

    __unsafe_unretained ChromaProcessTap *tap = self;
    status = AudioDeviceCreateIOProcIDWithBlock(
        &_ioProcID,
        _aggregateDeviceID,
        NULL,
        ^(
            const AudioTimeStamp *inNow,
            const AudioBufferList *input,
            const AudioTimeStamp *inputTime,
            AudioBufferList *output,
            const AudioTimeStamp *outputTime
        ) {
            (void)inNow;
            (void)output;
            (void)outputTime;
            [tap consumeInput:input inputTime:inputTime];
        }
    );
    if (status != noErr) {
        if (error) *error = ChromaOSStatusMessage(@"AudioDeviceCreateIOProcIDWithBlock", status);
        [self invalidate];
        return NO;
    }

    status = AudioDeviceStart(_aggregateDeviceID, _ioProcID);
    if (status != noErr) {
        if (error) *error = ChromaOSStatusMessage(@"AudioDeviceStart", status);
        [self invalidate];
        return NO;
    }
    return YES;
}

- (void)consumeInput:(const AudioBufferList *)input
           inputTime:(const AudioTimeStamp *)inputTime {
    if (input == NULL || input->mNumberBuffers == 0 || _callback == NULL) return;

    BOOL nonInterleaved = (_format.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0;
    uint32_t frames = 0;
    const float *left = NULL;
    const float *right = NULL;

    if (nonInterleaved) {
        const AudioBuffer *leftBuffer = &input->mBuffers[0];
        frames = leftBuffer->mDataByteSize / sizeof(float);
        left = leftBuffer->mData;
        if (input->mNumberBuffers > 1) {
            const AudioBuffer *rightBuffer = &input->mBuffers[1];
            frames = MIN(frames, rightBuffer->mDataByteSize / sizeof(float));
            right = rightBuffer->mData;
        } else {
            right = left;
        }
    } else {
        const AudioBuffer *buffer = &input->mBuffers[0];
        uint32_t channels = MAX(_format.mChannelsPerFrame, 1u);
        frames = buffer->mDataByteSize / (sizeof(float) * channels);
        frames = MIN(frames, kChromaMaximumFrames);
        const float *samples = buffer->mData;
        if (samples == NULL) return;
        for (uint32_t frame = 0; frame < frames; frame++) {
            _leftScratch[frame] = samples[frame * channels];
            _rightScratch[frame] = channels > 1
                ? samples[frame * channels + 1]
                : _leftScratch[frame];
        }
        left = _leftScratch;
        right = _rightScratch;
    }

    frames = MIN(frames, kChromaMaximumFrames);
    if (frames == 0 || left == NULL || right == NULL) return;
    double sampleTime = inputTime == NULL ? 0.0 : inputTime->mSampleTime;
    _callback(left, right, frames, _format.mSampleRate, sampleTime, _context);
}

- (void)invalidate {
    if (_aggregateDeviceID != kAudioObjectUnknown) {
        if (_ioProcID != NULL) {
            AudioDeviceStop(_aggregateDeviceID, _ioProcID);
            AudioDeviceDestroyIOProcID(_aggregateDeviceID, _ioProcID);
            _ioProcID = NULL;
        }
        AudioHardwareDestroyAggregateDevice(_aggregateDeviceID);
        _aggregateDeviceID = kAudioObjectUnknown;
    }
    if (_tapID != kAudioObjectUnknown) {
        AudioHardwareDestroyProcessTap(_tapID);
        _tapID = kAudioObjectUnknown;
    }
    _callback = NULL;
    _context = NULL;
}

@end

void *chroma_process_tap_start(
    ChromaAudioCallback callback,
    void *context,
    double *sampleRate,
    char *errorBuffer,
    size_t errorBufferSize
) {
    @autoreleasepool {
        ChromaProcessTap *tap = [[ChromaProcessTap alloc] init];
        if (tap == nil) {
            if (errorBuffer && errorBufferSize) {
                strlcpy(errorBuffer, "could not allocate Process Tap", errorBufferSize);
            }
            return NULL;
        }
        NSString *error = nil;
        if (![tap startWithCallback:callback context:context error:&error]) {
            if (errorBuffer && errorBufferSize) {
                strlcpy(errorBuffer, error.UTF8String ?: "unknown Process Tap error", errorBufferSize);
            }
            return NULL;
        }
        if (sampleRate) *sampleRate = tap.sampleRate;
        return (__bridge_retained void *)tap;
    }
}

void chroma_process_tap_stop(void *handle) {
    if (handle == NULL) return;
    @autoreleasepool {
        ChromaProcessTap *tap = CFBridgingRelease(handle);
        [tap invalidate];
    }
}
