#import <CoreFoundation/CoreFoundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <Vision/Vision.h>

#include <math.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

typedef void (*THD75VisionObservationCallback)(
    void *context,
    const uint8_t *utf8,
    size_t utf8_len,
    float confidence,
    float x,
    float y,
    float width,
    float height);

enum {
    THD75VisionStatusOk = 0,
    THD75VisionStatusInvalidArguments = 1,
    THD75VisionStatusUnsupported = 2,
    THD75VisionStatusImageCreation = 3,
    THD75VisionStatusRequest = 4,
    THD75VisionStatusException = 5,
    THD75VisionStatusInvalidObservation = 6,
};

static void THD75VisionWriteError(
    uint8_t *buffer,
    size_t capacity,
    NSString *message) {
    if (buffer == NULL || capacity == 0) {
        return;
    }
    buffer[0] = '\0';
    if (message == nil) {
        return;
    }

    NSData *encoded = [message dataUsingEncoding:NSUTF8StringEncoding];
    if (encoded == nil) {
        return;
    }
    size_t encodedLength = (size_t)encoded.length;
    size_t copyLength = encodedLength < capacity - 1 ? encodedLength : capacity - 1;
    if (copyLength > 0) {
        memcpy(buffer, encoded.bytes, copyLength);
    }
    buffer[copyLength] = '\0';
}

static CGFloat THD75VisionClampUnit(CGFloat value) {
    if (value < 0.0) {
        return 0.0;
    }
    if (value > 1.0) {
        return 1.0;
    }
    return value;
}

int32_t thd75_vision_recognize_rgb888(
    const uint8_t *rgb,
    size_t rgb_len,
    size_t width,
    size_t height,
    size_t bytes_per_row,
    THD75VisionObservationCallback callback,
    void *context,
    uint8_t *error_buffer,
    size_t error_capacity) {
    if (error_buffer != NULL && error_capacity > 0) {
        error_buffer[0] = '\0';
    }
    if (rgb == NULL || callback == NULL || context == NULL ||
        error_buffer == NULL || error_capacity == 0 ||
        !((width == 240 && height == 180) ||
          (width == 960 && height == 720)) ||
        width > SIZE_MAX / 3 || bytes_per_row != width * 3 ||
        height > SIZE_MAX / bytes_per_row ||
        rgb_len != bytes_per_row * height) {
        THD75VisionWriteError(
            error_buffer,
            error_capacity,
            @"invalid RGB888 buffer, geometry, callback, context, or error capacity");
        return THD75VisionStatusInvalidArguments;
    }

    if (@available(macOS 10.15, *)) {
        __block int32_t status = THD75VisionStatusOk;
        @autoreleasepool {
            CFDataRef pixelData = NULL;
            CGDataProviderRef provider = NULL;
            CGColorSpaceRef colorSpace = NULL;
            CGImageRef image = NULL;

            @try {
                pixelData = CFDataCreate(
                    kCFAllocatorDefault,
                    (const UInt8 *)rgb,
                    (CFIndex)rgb_len);
                if (pixelData == NULL) {
                    THD75VisionWriteError(
                        error_buffer, error_capacity, @"could not copy RGB888 pixels");
                    status = THD75VisionStatusImageCreation;
                }

                if (status == THD75VisionStatusOk) {
                    provider = CGDataProviderCreateWithCFData(pixelData);
                    colorSpace = CGColorSpaceCreateDeviceRGB();
                    if (provider == NULL || colorSpace == NULL) {
                        THD75VisionWriteError(
                            error_buffer,
                            error_capacity,
                            @"could not create CoreGraphics RGB resources");
                        status = THD75VisionStatusImageCreation;
                    }
                }

                if (status == THD75VisionStatusOk) {
                    CGBitmapInfo bitmapInfo =
                        (CGBitmapInfo)(kCGImageAlphaNone | kCGBitmapByteOrderDefault);
                    image = CGImageCreate(
                        width,
                        height,
                        8,
                        24,
                        bytes_per_row,
                        colorSpace,
                        bitmapInfo,
                        provider,
                        NULL,
                        false,
                        kCGRenderingIntentDefault);
                    if (image == NULL) {
                        THD75VisionWriteError(
                            error_buffer,
                            error_capacity,
                            @"CoreGraphics rejected the fixed RGB888 image");
                        status = THD75VisionStatusImageCreation;
                    }
                }

                if (status == THD75VisionStatusOk) {
                    VNRecognizeTextRequest *request =
                        [[VNRecognizeTextRequest alloc] init];
                    request.recognitionLevel = VNRequestTextRecognitionLevelAccurate;
                    request.usesLanguageCorrection = NO;
                    request.minimumTextHeight = 0.0;

                    VNImageRequestHandler *handler =
                        [[VNImageRequestHandler alloc] initWithCGImage:image options:@{}];
                    NSError *requestError = nil;
                    BOOL performed = [handler performRequests:@[ request ] error:&requestError];
                    if (!performed) {
                        NSString *detail = requestError.localizedDescription;
                        THD75VisionWriteError(
                            error_buffer,
                            error_capacity,
                            detail != nil ? detail : @"Vision text request failed");
                        status = THD75VisionStatusRequest;
                    }

                    if (status == THD75VisionStatusOk) {
                        for (VNRecognizedTextObservation *observation in request.results) {
                            VNRecognizedText *candidate =
                                [observation topCandidates:1].firstObject;
                            if (candidate == nil || candidate.string.length == 0) {
                                continue;
                            }

                            CGRect box = CGRectStandardize(observation.boundingBox);
                            CGFloat left = CGRectGetMinX(box);
                            CGFloat right = CGRectGetMaxX(box);
                            CGFloat visionBottom = CGRectGetMinY(box);
                            CGFloat visionTop = CGRectGetMaxY(box);
                            float confidence = candidate.confidence;
                            if (!isfinite(left) || !isfinite(right) ||
                                !isfinite(visionBottom) || !isfinite(visionTop) ||
                                !isfinite(confidence)) {
                                THD75VisionWriteError(
                                    error_buffer,
                                    error_capacity,
                                    @"Vision returned non-finite observation geometry");
                                status = THD75VisionStatusInvalidObservation;
                                break;
                            }

                            left = THD75VisionClampUnit(left);
                            right = THD75VisionClampUnit(right);
                            visionBottom = THD75VisionClampUnit(visionBottom);
                            visionTop = THD75VisionClampUnit(visionTop);
                            CGFloat top = 1.0 - visionTop;
                            CGFloat bottom = 1.0 - visionBottom;
                            CGFloat normalizedWidth = right - left;
                            CGFloat normalizedHeight = bottom - top;
                            if (normalizedWidth <= 0.0 || normalizedHeight <= 0.0) {
                                THD75VisionWriteError(
                                    error_buffer,
                                    error_capacity,
                                    @"Vision returned an empty observation rectangle");
                                status = THD75VisionStatusInvalidObservation;
                                break;
                            }

                            NSData *text =
                                [candidate.string dataUsingEncoding:NSUTF8StringEncoding];
                            if (text == nil || text.length == 0) {
                                THD75VisionWriteError(
                                    error_buffer,
                                    error_capacity,
                                    @"Vision text could not be encoded as UTF-8");
                                status = THD75VisionStatusInvalidObservation;
                                break;
                            }

                            callback(
                                context,
                                (const uint8_t *)text.bytes,
                                (size_t)text.length,
                                confidence,
                                (float)left,
                                (float)top,
                                (float)normalizedWidth,
                                (float)normalizedHeight);
                        }
                    }
                }
            } @catch (NSException *exception) {
                NSString *detail = [NSString stringWithFormat:
                    @"%@ exception: %@",
                    exception.name,
                    exception.reason != nil ? exception.reason : @"no reason"];
                THD75VisionWriteError(error_buffer, error_capacity, detail);
                status = THD75VisionStatusException;
            } @finally {
                if (image != NULL) {
                    CGImageRelease(image);
                }
                if (colorSpace != NULL) {
                    CGColorSpaceRelease(colorSpace);
                }
                if (provider != NULL) {
                    CGDataProviderRelease(provider);
                }
                if (pixelData != NULL) {
                    CFRelease(pixelData);
                }
            }
        }
        return status;
    }

    THD75VisionWriteError(
        error_buffer,
        error_capacity,
        @"macOS Vision text recognition requires macOS 10.15 or newer");
    return THD75VisionStatusUnsupported;
}
