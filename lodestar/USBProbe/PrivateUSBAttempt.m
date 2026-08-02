#import "PrivateUSBAttempt.h"

#import <IOKit/IOKitLib.h>
#import <dlfcn.h>

/// Minimal redeclaration of Apple's PRIVATE IOUSBHost interface so the
/// compiler knows the selector signature. The class itself is resolved at
/// runtime with NSClassFromString after dlopen, so nothing links against
/// the private framework.
typedef void (^LSInterestHandler)(id hostObject, uint32_t messageType, void *messageArgument);

@interface LSIOUSBHostObject : NSObject
- (nullable instancetype)initWithIOService:(io_service_t)ioService
                                   options:(NSUInteger)options
                                     queue:(nullable dispatch_queue_t)queue
                                     error:(NSError **)error
                           interestHandler:(nullable LSInterestHandler)interestHandler;
@end

@implementation PrivateUSBAttempt

+ (NSArray<NSString *> *)dumpPropertiesForClass:(NSString *)className
                                          limit:(NSUInteger)limit {
    NSMutableArray<NSString *> *lines = [NSMutableArray array];
    CFMutableDictionaryRef matching = IOServiceMatching(className.UTF8String);
    if (matching == NULL) {
        [lines addObject:[NSString stringWithFormat:@"  %@: IOServiceMatching nil", className]];
        return lines;
    }
    io_iterator_t iterator = IO_OBJECT_NULL;
    kern_return_t kr = IOServiceGetMatchingServices(kIOMainPortDefault, matching, &iterator);
    if (kr != KERN_SUCCESS) {
        [lines addObject:[NSString stringWithFormat:@"  %@: matching failed 0x%08x", className, kr]];
        return lines;
    }

    // Keys that identify a USB device; everything else is summarized by
    // count so the console stays readable.
    NSArray<NSString *> *interesting = @[
        @"idVendor", @"idProduct", @"bcdDevice", @"bDeviceClass", @"bDeviceSubClass",
        @"USB Product Name", @"USB Vendor Name", @"USB Serial Number",
        @"kUSBSerialNumberString", @"kUSBProductString", @"kUSBVendorString",
        @"IOClass", @"IOProviderClass", @"locationID", @"PortNum", @"bInterfaceNumber",
        @"bInterfaceClass", @"bInterfaceSubClass", @"bInterfaceProtocol"
    ];

    NSUInteger index = 0;
    io_service_t service = IO_OBJECT_NULL;
    while ((service = IOIteratorNext(iterator)) != IO_OBJECT_NULL && index < limit) {
        CFMutableDictionaryRef properties = NULL;
        kern_return_t pkr =
            IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0);
        if (pkr == KERN_SUCCESS && properties != NULL) {
            NSDictionary *dict = (__bridge_transfer NSDictionary *)properties;
            [lines addObject:[NSString stringWithFormat:@"  %@[%lu]: %lu properties readable",
                                                       className, (unsigned long)index,
                                                       (unsigned long)dict.count]];
            // A sandbox-filtered dict is tiny; list it whole so we learn
            // exactly which keys survive rather than guessing.
            if (dict.count <= 8) {
                for (NSString *key in dict) {
                    [lines addObject:[NSString stringWithFormat:@"      (all) %@ = %@",
                                                               key, dict[key]]];
                }
            }
            for (NSString *key in interesting) {
                id value = dict[key];
                if (value != nil) {
                    [lines addObject:[NSString stringWithFormat:@"      %@ = %@", key, value]];
                }
            }
        } else {
            [lines addObject:[NSString stringWithFormat:@"  %@[%lu]: properties DENIED 0x%08x",
                                                       className, (unsigned long)index, pkr]];
        }
        // Registry path is itself informative (shows the USB topology).
        io_string_t path = {0};
        if (IORegistryEntryGetPath(service, kIOServicePlane, path) == KERN_SUCCESS) {
            [lines addObject:[NSString stringWithFormat:@"      path = %s", path]];
        }
        IOObjectRelease(service);
        index++;
    }
    while ((service = IOIteratorNext(iterator)) != IO_OBJECT_NULL) {
        IOObjectRelease(service);
    }
    IOObjectRelease(iterator);
    if (index == 0) {
        [lines addObject:[NSString stringWithFormat:@"  %@: no services matched", className]];
    }
    return lines;
}

+ (NSArray<NSString *> *)attemptPrivateHostDeviceOpen {
    NSMutableArray<NSString *> *lines =
        [NSMutableArray arrayWithObject:@"[private] instantiate IOUSBHostDevice on a real device:"];

    void *handle = dlopen("/System/Library/PrivateFrameworks/IOUSBHost.framework/IOUSBHost", RTLD_NOW);
    if (handle == NULL) {
        [lines addObject:[NSString stringWithFormat:@"  dlopen failed: %s", dlerror()]];
        return lines;
    }
    Class cls = NSClassFromString(@"IOUSBHostDevice");
    if (cls == Nil) {
        [lines addObject:@"  IOUSBHostDevice class not resolvable"];
        return lines;
    }

    CFMutableDictionaryRef matching = IOServiceMatching("IOUSBHostDevice");
    io_iterator_t iterator = IO_OBJECT_NULL;
    if (matching == NULL ||
        IOServiceGetMatchingServices(kIOMainPortDefault, matching, &iterator) != KERN_SUCCESS) {
        [lines addObject:@"  could not enumerate IOUSBHostDevice services"];
        return lines;
    }
    io_service_t service = IOIteratorNext(iterator);
    IOObjectRelease(iterator);
    if (service == IO_OBJECT_NULL) {
        [lines addObject:@"  no IOUSBHostDevice service present (plug the radio in)"];
        return lines;
    }

    NSError *error = nil;
    id instance = nil;
    @try {
        instance = [[cls alloc] initWithIOService:service
                                          options:0
                                            queue:NULL
                                            error:&error
                                  interestHandler:NULL];
    } @catch (NSException *exception) {
        [lines addObject:[NSString stringWithFormat:@"  init raised: %@: %@",
                                                   exception.name, exception.reason]];
        IOObjectRelease(service);
        return lines;
    }
    if (instance != nil) {
        [lines addObject:@"  !! init SUCCEEDED: raw USB reachable from a sandboxed app"];
    } else {
        [lines addObject:[NSString stringWithFormat:@"  init returned nil, error = %@",
                                                   error ?: @"(none)"]];
    }
    IOObjectRelease(service);
    return lines;
}

+ (void)appendSubtreeOf:(io_service_t)service
                  depth:(NSUInteger)depth
                  lines:(NSMutableArray<NSString *> *)lines {
    if (depth > 4) {
        return;
    }
    io_iterator_t children = IO_OBJECT_NULL;
    if (IORegistryEntryGetChildIterator(service, kIOServicePlane, &children) != KERN_SUCCESS) {
        return;
    }
    io_service_t child = IO_OBJECT_NULL;
    while ((child = IOIteratorNext(children)) != IO_OBJECT_NULL) {
        io_name_t name = {0};
        io_name_t className = {0};
        IORegistryEntryGetName(child, name);
        IOObjectGetClass(child, className);
        NSMutableString *indent = [NSMutableString string];
        for (NSUInteger i = 0; i < depth; i++) {
            [indent appendString:@"  "];
        }
        [lines addObject:[NSString stringWithFormat:@"    %@|- %s  [class: %s]",
                                                   indent, name, className]];
        [self appendSubtreeOf:child depth:depth + 1 lines:lines];
        IOObjectRelease(child);
    }
    IOObjectRelease(children);
}

+ (NSArray<NSString *> *)dumpUSBDeviceTree {
    NSMutableArray<NSString *> *lines = [NSMutableArray
        arrayWithObject:@"[IOKit] USB device tree (which driver claimed which interface?):"];
    CFMutableDictionaryRef matching = IOServiceMatching("IOUSBHostDevice");
    io_iterator_t iterator = IO_OBJECT_NULL;
    if (matching == NULL ||
        IOServiceGetMatchingServices(kIOMainPortDefault, matching, &iterator) != KERN_SUCCESS) {
        [lines addObject:@"  could not enumerate USB devices"];
        return lines;
    }
    io_service_t device = IO_OBJECT_NULL;
    BOOL any = NO;
    while ((device = IOIteratorNext(iterator)) != IO_OBJECT_NULL) {
        any = YES;
        io_name_t name = {0};
        io_name_t className = {0};
        IORegistryEntryGetName(device, name);
        IOObjectGetClass(device, className);
        [lines addObject:[NSString stringWithFormat:@"  %s  [class: %s]", name, className]];
        [self appendSubtreeOf:device depth:1 lines:lines];
        IOObjectRelease(device);
    }
    IOObjectRelease(iterator);
    if (!any) {
        [lines addObject:@"  no USB devices attached"];
    }
    [lines addObject:@"  (a bare IOUSBHostInterface leaf = NO iOS driver claimed that interface)"];
    return lines;
}

@end
