// No-radio fixture for the complete selected-device failure, native cleanup,
// failure framing and helper exit path. All device/channel objects are local
// NSObject substitutes; no Bluetooth enumeration or implementation is invoked.
static double g_failure_fixture_time;
static BOOL g_failure_fixture_confirm_close;
static unsigned g_failure_fixture_opens;
static unsigned g_failure_fixture_closes;

static double failure_fixture_now(void) { return g_failure_fixture_time; }
static SInt32 failure_fixture_pump(double seconds) {
    (void)seconds;
    g_failure_fixture_time = g_failure_fixture_opens ? 1.0 : 0.05;
    return kCFRunLoopRunTimedOut;
}

@interface FailureTestChannel : NSObject {
    __weak id _delegate;
}
- (IOReturn)setDelegate:(id)delegate;
- (IOReturn)closeChannel;
- (BluetoothRFCOMMChannelID)getChannelID;
@end
@implementation FailureTestChannel
- (IOReturn)setDelegate:(id)delegate { _delegate = delegate; return kIOReturnSuccess; }
- (IOReturn)closeChannel {
    g_failure_fixture_closes++;
    if (g_failure_fixture_confirm_close) {
        [_delegate rfcommChannelClosed:(IOBluetoothRFCOMMChannel *)self];
    }
    return kIOReturnSuccess;
}
- (BluetoothRFCOMMChannelID)getChannelID { return 27; }
@end

@interface FailureTestDevice : NSObject
- (BOOL)isConnected;
- (NSString *)addressString;
- (IOReturn)performSDPQuery:(id)target;
- (IOReturn)openRFCOMMChannelAsync:(IOBluetoothRFCOMMChannel **)channel
                   withChannelID:(BluetoothRFCOMMChannelID)channel_id
                        delegate:(id)delegate;
@end
@implementation FailureTestDevice
- (BOOL)isConnected { return YES; }
- (NSString *)addressString { return @"7c-b8-da-a0-e2-75"; }
- (IOReturn)performSDPQuery:(id)target {
    return target == nil ? kIOReturnSuccess : kIOReturnBadArgument;
}
- (IOReturn)openRFCOMMChannelAsync:(IOBluetoothRFCOMMChannel **)channel
                   withChannelID:(BluetoothRFCOMMChannelID)channel_id
                        delegate:(id)delegate {
    if (channel_id != 27) return kIOReturnBadArgument;
    g_failure_fixture_opens++;
    FailureTestChannel *created = [[FailureTestChannel alloc] init];
    [created setDelegate:delegate];
    *channel = (IOBluetoothRFCOMMChannel *)created;
    // No open callback: the real pipeline must reach its absolute deadline.
    return kIOReturnSuccess;
}
@end

static int test_open_failure_cleanup(BOOL confirm_close) {
    g_failure_fixture_time = 0.0;
    g_failure_fixture_confirm_close = confirm_close;
    g_failure_fixture_opens = 0;
    g_failure_fixture_closes = 0;
    const NativeOpenRuntime runtime = {
        .now = failure_fixture_now,
        .pump = failure_fixture_pump,
    };
    int failure = 0;
    @autoreleasepool {
        FailureTestDevice *device = [[FailureTestDevice alloc] init];
        RfcommContext *ctx = open_selected_device(
            (IOBluetoothDevice *)device, 27, NO, &failure, 1.0, &runtime
        );
        if (ctx) { destroy_rfcomm_context(ctx); return 120; }
        if (failure != BT_HELPER_EXIT_RFCOMM_DEADLINE ||
            g_failure_fixture_opens != 1 || g_failure_fixture_closes != 1 ||
            (g_unconfirmed_close != NULL) == confirm_close) return 121;
    }
    return report_failed_open(failure);
}
