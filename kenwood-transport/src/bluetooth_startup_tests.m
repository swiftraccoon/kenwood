// Included after the selected-device pipeline. These NSObject fixtures never
// enumerate devices or invoke an IOBluetooth implementation. They exercise
// production ordering, deadline admission, callbacks, and normal cleanup.
typedef struct {
    double now;
    double pump_advance;
    double sdp_advance;
    double requested_slice;
    SInt32 pump_result;
    IOReturn open_status;
    unsigned pumps;
    unsigned sdp_calls;
    unsigned opens;
    unsigned closes;
    unsigned sequence;
    BOOL invalid;
} StartupFixture;

static StartupFixture g_startup_fixture;

static double startup_fixture_now(void) {
    return g_startup_fixture.now;
}

static SInt32 startup_fixture_pump(double seconds) {
    StartupFixture *fixture = &g_startup_fixture;
    fixture->pumps++;
    fixture->invalid |= fixture->sequence != 0 || seconds <= 0.0 || seconds > 0.05;
    fixture->sequence = 1;
    fixture->requested_slice = seconds;
    fixture->now += fixture->pump_advance;
    return fixture->pump_result;
}

@interface StartupTestChannel : NSObject {
    __weak id _delegate;
}
- (IOReturn)setDelegate:(id)delegate;
- (IOReturn)closeChannel;
- (BluetoothRFCOMMChannelID)getChannelID;
@end

@implementation StartupTestChannel
- (IOReturn)setDelegate:(id)delegate {
    _delegate = delegate;
    return kIOReturnSuccess;
}
- (IOReturn)closeChannel {
    g_startup_fixture.closes++;
    [_delegate rfcommChannelClosed:(IOBluetoothRFCOMMChannel *)self];
    return kIOReturnSuccess;
}
- (BluetoothRFCOMMChannelID)getChannelID {
    return 27;
}
@end

@interface StartupTestDevice : NSObject
- (BOOL)isConnected;
- (NSString *)addressString;
- (IOReturn)performSDPQuery:(id)target;
- (IOReturn)openRFCOMMChannelAsync:(IOBluetoothRFCOMMChannel **)channel
                   withChannelID:(BluetoothRFCOMMChannelID)channel_id
                        delegate:(id)delegate;
@end

@implementation StartupTestDevice
- (BOOL)isConnected {
    return YES;
}
- (NSString *)addressString {
    return @"7c-b8-da-a0-e2-75";
}
- (IOReturn)performSDPQuery:(id)target {
    StartupFixture *fixture = &g_startup_fixture;
    fixture->sdp_calls++;
    fixture->invalid |= target != nil || fixture->sequence != 1;
    fixture->sequence = 2;
    fixture->now += fixture->sdp_advance;
    return kIOReturnSuccess;
}
- (IOReturn)openRFCOMMChannelAsync:(IOBluetoothRFCOMMChannel **)channel
                   withChannelID:(BluetoothRFCOMMChannelID)channel_id
                        delegate:(id)delegate {
    StartupFixture *fixture = &g_startup_fixture;
    fixture->opens++;
    fixture->invalid |= channel_id != 27 || fixture->sequence != 2;
    fixture->sequence = 3;
    StartupTestChannel *created = [[StartupTestChannel alloc] init];
    [created setDelegate:delegate];
    *channel = (IOBluetoothRFCOMMChannel *)created;
    [delegate rfcommChannelOpenComplete:*channel status:fixture->open_status];
    return kIOReturnSuccess;
}
@end

static BOOL startup_fixture_case(double initial_time, double pump_advance,
                                double sdp_advance, SInt32 pump_result,
                                IOReturn open_status, int expected_failure,
                                unsigned expected_pumps, unsigned expected_sdp,
                                unsigned expected_opens) {
    g_startup_fixture = (StartupFixture){
        .now = initial_time,
        .pump_advance = pump_advance,
        .sdp_advance = sdp_advance,
        .pump_result = pump_result,
        .open_status = open_status,
    };
    const NativeOpenRuntime runtime = {
        .now = startup_fixture_now,
        .pump = startup_fixture_pump,
    };
    BOOL accepted = NO;
    BOOL retired = YES;
    int failure = 0;
    @autoreleasepool {
        StartupTestDevice *device = [[StartupTestDevice alloc] init];
        RfcommContext *ctx = open_selected_device(
            (IOBluetoothDevice *)device, 27, NO, &failure, 1.0, &runtime
        );
        accepted = ctx != NULL;
        if (ctx) retired = destroy_rfcomm_context(ctx);
    }
    StartupFixture *fixture = &g_startup_fixture;
    return !fixture->invalid && retired && !g_unconfirmed_close &&
        accepted == (expected_failure == 0) &&
        (accepted || failure == expected_failure) &&
        fixture->pumps == expected_pumps && fixture->sdp_calls == expected_sdp &&
        fixture->opens == expected_opens && fixture->closes == expected_opens;
}

static int test_startup_progress(void) {
    // An already-connected device must still observe startup processing
    // before its nil-target SDP and single fixed-channel opening.
    if (!startup_fixture_case(0.0, 0.05, 0.0, kCFRunLoopRunTimedOut,
            kIOReturnSuccess, 0, 1, 1, 1)) return 110;
    if (g_startup_fixture.requested_slice != 0.05) return 111;

    if (!startup_fixture_case(1.0, 0.0, 0.0, kCFRunLoopRunFinished,
            kIOReturnSuccess, BT_HELPER_EXIT_STARTUP_DEADLINE, 0, 0, 0)) return 112;
    if (!startup_fixture_case(0.0, 1.0, 0.0, kCFRunLoopRunTimedOut,
            kIOReturnSuccess, BT_HELPER_EXIT_STARTUP_DEADLINE, 1, 0, 0)) return 113;
    if (!startup_fixture_case(0.0, 1.5, 0.0, kCFRunLoopRunTimedOut,
            kIOReturnSuccess, BT_HELPER_EXIT_STARTUP_DEADLINE, 1, 0, 0)) return 114;

    // A run loop can finish early. Its return is an ordering opportunity,
    // not proof of readiness: acceptance still requires the channel callback.
    if (!startup_fixture_case(0.0, 0.0, 0.0, kCFRunLoopRunFinished,
            kIOReturnSuccess, 0, 1, 1, 1)) return 115;
    if (!startup_fixture_case(0.0, 0.0, 0.0, kCFRunLoopRunFinished,
            kIOReturnError, BT_HELPER_EXIT_RFCOMM_COMPLETION, 1, 1, 1)) return 116;

    // The remaining budget bounds the requested slice; processing that uses
    // exactly that budget must prevent both subsequent operation dispatches.
    if (!startup_fixture_case(0.96875, 0.03125, 0.0, kCFRunLoopRunTimedOut,
            kIOReturnSuccess, BT_HELPER_EXIT_STARTUP_DEADLINE, 1, 0, 0)) return 117;
    if (g_startup_fixture.requested_slice != 0.03125) return 118;

    // Dispatching SDP does not grant permission to open a channel after the
    // shared absolute deadline expires during that operation.
    if (!startup_fixture_case(0.0, 0.0, 1.0, kCFRunLoopRunFinished,
            kIOReturnSuccess, BT_HELPER_EXIT_SDP_DEADLINE, 1, 1, 0)) return 119;
    return 0;
}
