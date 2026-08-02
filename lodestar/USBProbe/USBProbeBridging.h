// Bridges the IOKit user-client C API into Swift. The iOS SDK ships these
// symbols (IOServiceGetMatchingServices / IOServiceOpen / IORegistry*) but
// no Swift `IOKit` module; `import IOKit` fails on iOS, exactly as in the
// LodestarIPad target. The control-path probe uses them to attempt raw USB
// access from the app sandbox and observe precisely how iOS denies it.
#import <IOKit/IOKitLib.h>
#import <IOKit/IOReturn.h>

#import "PrivateUSBAttempt.h"
