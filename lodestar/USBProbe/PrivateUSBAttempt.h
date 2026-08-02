#import <Foundation/Foundation.h>

NS_ASSUME_NONNULL_BEGIN

/// Attempts that need Objective-C message dispatch: dumping IORegistry
/// properties of USB services, and instantiating Apple's PRIVATE
/// `IOUSBHostDevice` class obtained at runtime via `NSClassFromString`.
///
/// This exists to answer empirically what a sandboxed iPhone app can do
/// with an attached USB device. It is a local diagnostic only: the private
/// class use here must never ship in a submitted app.
@interface PrivateUSBAttempt : NSObject

/// Every IORegistry property of each matched service of `className`
/// (idVendor / idProduct / product strings identify the radio if readable).
+ (NSArray<NSString *> *)dumpPropertiesForClass:(NSString *)className
                                          limit:(NSUInteger)limit;

/// Tries `-[IOUSBHostDevice initWithIOService:options:queue:error:interestHandler:]`
/// against the first matching USB device and reports the exact outcome.
+ (NSArray<NSString *> *)attemptPrivateHostDeviceOpen;

/// Walks the IORegistry subtree under each attached USB device, printing
/// every node's name and its actual IOKit class. This reveals WHICH iOS
/// driver claimed which USB interface: a claimed interface has a driver
/// node beneath it (the audio interfaces do), an unclaimed one is a bare
/// `IOUSBHostInterface` leaf. That distinction is the whole reason audio
/// is reachable from an app and CDC serial is not.
+ (NSArray<NSString *> *)dumpUSBDeviceTree;

@end

NS_ASSUME_NONNULL_END
