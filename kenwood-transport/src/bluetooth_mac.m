#ifdef __APPLE__

#import <Foundation/Foundation.h>
#import <IOBluetooth/IOBluetooth.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

// IOBluetooth's RFCOMM writes are not cancellable in-process. On current
// macOS, writeSync: can remain in CFWriteStreamWrite forever when the peer
// stops granting flow-control credit. writeAsync: only moves that same
// unbounded call into a block on the main dispatch queue, where it wedges the
// host's CFRunLoop instead. Even the deprecated write:length:sleep:NO method
// is a runtime trampoline to writeSync: (the sleep argument is ignored).
//
// The hard boundary is therefore a helper *process*. The Rust parent spawns a
// signed executable containing this constructor with the private environment
// sentinel below. This constructor runs before main, owns every IOBluetooth
// object, exposes stdin/stdout as raw serial byte streams, and exits. If
// IOBluetooth wedges, the parent remains responsive and SIGKILLs this process
// during timeout or cleanup. Command-line clients can re-execute themselves;
// sandboxed applications provide a separately signed inheriting helper.

#define BT_HELPER_SENTINEL_ENV "KENWOOD_BT_HELPER_PROCESS_V2"
#define BT_HELPER_SENTINEL_VALUE "4d7f29c8b35a"
#define BT_HELPER_DEVICE_ENV "KENWOOD_BT_HELPER_DEVICE"
#define BT_HELPER_CHANNEL_ENV "KENWOOD_BT_HELPER_CHANNEL"
#define BT_HELPER_CONTROL_ENV "KENWOOD_BT_HELPER_CONTROL_MODE"
#define BT_HELPER_TEST_ENV "KENWOOD_BT_HELPER_TEST_MODE"
#define BT_HELPER_LIVENESS_FD_ENV "KENWOOD_BT_HELPER_LIVENESS_FD"
#define BT_HELPER_PRE_READY_CAPACITY 4096
#define BT_HELPER_MAX_PAIRED_DEVICES 64

static const uint8_t kReadyMagic[] = "KENWBT-READY-v2!";
static const uint8_t kOpenFailureMagic[] = "KENWBT-ERROR-v1!";

// The selected identifier matched more than one paired device name. Keep
// this distinct from 71, whose process-local open race is retried once.
#define BT_HELPER_EXIT_AMBIGUOUS_DEVICE_NAME 87
#define BT_HELPER_EXIT_TOO_MANY_PAIRED_DEVICES 88
#define BT_HELPER_EXIT_CLOSE_UNCONFIRMED 89

// Private exit statuses name the stage at which the helper failed on this
// host, not a cause in the peer. Keep them aligned with the Rust helper-exit
// decoder; a value it does not recognize is decoded as a helper failure. When
// close uncertainty selects the process exit status, the framed record still
// carries the original stage.
enum {
    BT_HELPER_EXIT_CONTEXT_ALLOCATION = 100,
    BT_HELPER_EXIT_SDP_START = 101,
    BT_HELPER_EXIT_SDP_COMPLETION = 102,
    BT_HELPER_EXIT_SDP_DEADLINE = 103,
    BT_HELPER_EXIT_SERVICE_RESOLUTION = 104,
    BT_HELPER_EXIT_RFCOMM_START = 105,
    BT_HELPER_EXIT_RFCOMM_COMPLETION = 106,
    BT_HELPER_EXIT_RFCOMM_DEADLINE = 107,
    BT_HELPER_EXIT_RFCOMM_ENDPOINT = 108,
    BT_HELPER_EXIT_STARTUP_DEADLINE = 109,
};

// Opt-in shim tracing. Set KENWOOD_BT_TRACE=1 on the parent. The helper
// inherits stderr, so every line carries its PID and can be correlated with
// the Rust transport log without contaminating the raw stdout byte stream.
static _Atomic int g_bt_trace = -1;

static int bt_trace_enabled(void) {
    int value = g_bt_trace;
    if (value < 0) {
        const char *env = getenv("KENWOOD_BT_TRACE");
        value = (env && env[0] && env[0] != '0') ? 1 : 0;
        g_bt_trace = value;
    }
    return value;
}

static void bt_trace(const char *format, ...)
    __attribute__((format(printf, 1, 2)));
static void bt_trace(const char *format, ...) {
    if (!bt_trace_enabled()) return;
    struct timeval time;
    gettimeofday(&time, NULL);
    struct tm broken_down;
    gmtime_r(&time.tv_sec, &broken_down);
    fprintf(stderr,
            "[bt-helper pid=%d] %02d:%02d:%02d.%06d ",
            getpid(), broken_down.tm_hour, broken_down.tm_min,
            broken_down.tm_sec, (int)time.tv_usec);
    va_list arguments;
    va_start(arguments, format);
    vfprintf(stderr, format, arguments);
    va_end(arguments);
    fputc('\n', stderr);
    fflush(stderr);
}

// Referenced by Rust solely to force this Objective-C object (and therefore
// its constructor) out of the static archive into every using executable.
void bt_helper_link_anchor(void) {}

// std::process pipes do not expose a stable Rust API for O_NONBLOCK. Keep the
// platform constant and fcntl call in the native shim.
int bt_fd_set_nonblocking(int fd) {
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0) return -1;
    return fcntl(fd, F_SETFL, flags | O_NONBLOCK);
}

// Create the dedicated parent-liveness pipe with close-on-exec on both ends.
// The Rust child's pre-exec hook explicitly duplicates only the read end onto
// its fixed descriptor; the helper therefore cannot accidentally inherit a
// writer that would mask parent death.
int bt_liveness_pipe_create(int *read_fd, int *write_fd) {
    if (!read_fd || !write_fd) {
        errno = EINVAL;
        return -1;
    }
    int fds[2];
    if (pipe(fds) != 0) return -1;
    int read_flags = fcntl(fds[0], F_GETFD);
    int write_flags = fcntl(fds[1], F_GETFD);
    if (read_flags < 0 || write_flags < 0 ||
        fcntl(fds[0], F_SETFD, read_flags | FD_CLOEXEC) != 0 ||
        fcntl(fds[1], F_SETFD, write_flags | FD_CLOEXEC) != 0) {
        int saved_errno = errno;
        close(fds[0]);
        close(fds[1]);
        errno = saved_errno;
        return -1;
    }
    *read_fd = fds[0];
    *write_fd = fds[1];
    return 0;
}

// Called only by Command::pre_exec. Keep this async-signal-safe: dup2 and
// fcntl are the only operations performed between fork and exec.
int bt_helper_prepare_liveness_fd(int source_fd, int target_fd) {
    if (source_fd < 0 || target_fd < 0) {
        errno = EBADF;
        return -1;
    }
    if (source_fd != target_fd && dup2(source_fd, target_fd) < 0) return -1;
    int flags = fcntl(target_fd, F_GETFD);
    if (flags < 0) return -1;
    return fcntl(target_fd, F_SETFD, flags & ~FD_CLOEXEC);
}

@class RfcommDelegate;

// The exact target and completion status belong to this one helper query.
// A connected baseband or cached service list is not callback completion.
@interface SdpQueryDelegate : NSObject {
@public
    IOBluetoothDevice *device;
    _Atomic int state;
}
- (void)sdpQueryComplete:(IOBluetoothDevice *)completed_device status:(IOReturn)status;
@end

// SDP has no query-cancellation contract. Retain its callback target through
// this disposable helper's entire lifetime, including timeout and failure.
static SdpQueryDelegate *g_pending_sdp = nil;

@implementation SdpQueryDelegate
- (void)sdpQueryComplete:(IOBluetoothDevice *)completed_device status:(IOReturn)status {
    if (completed_device != device) {
        state = -1;
        return;
    }
    int expected = 0;
    int completed = (status == kIOReturnSuccess) ? 1 : -1;
    if (!atomic_compare_exchange_strong(&state, &expected, completed)) {
        bt_trace("duplicate SDP completion");
        _exit(90);
    }
    bt_trace("SDP complete status=0x%08x", (unsigned)status);
}
@end

typedef struct {
    IOBluetoothDevice *device;
    IOBluetoothRFCOMMChannel *channel;
    RfcommDelegate *delegate;
    int output_fd;
    _Atomic int state;
    _Atomic BOOL close_observed;
    uint8_t pre_ready[BT_HELPER_PRE_READY_CAPACITY];
    size_t pre_ready_length;
} RfcommContext;

@interface RfcommDelegate : NSObject <IOBluetoothRFCOMMChannelDelegate>
@property(nonatomic, assign) RfcommContext *ctx;
@end

static pthread_mutex_t g_context_mutex = PTHREAD_MUTEX_INITIALIZER;
// A timed-out native close has no cancellation guarantee. Keep its detached
// callback target and channel alive until this one-session helper exits.
static RfcommContext *g_unconfirmed_close = NULL;
static int write_all(int fd, const uint8_t *bytes, size_t length);

static double monotonic_seconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0.0;
    return (double)now.tv_sec + ((double)now.tv_nsec / 1000000000.0);
}

// The selected-device opening policy uses the same clock and event processor
// throughout its absolute budget, and carries that event processor through
// failure cleanup. Tests substitute a deterministic runtime without
// enumerating devices or initializing the Bluetooth framework.
typedef struct {
    double (*now)(void);
    SInt32 (*pump)(double seconds);
} NativeOpenRuntime;

static SInt32 pump_open_events(double seconds) {
    return CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, false);
}

static const NativeOpenRuntime kNativeOpenRuntime = {
    .now = monotonic_seconds,
    .pump = pump_open_events,
};

static BOOL process_startup_events(double deadline,
                                   const NativeOpenRuntime *runtime) {
    double remaining = deadline - runtime->now();
    if (remaining <= 0.0) return NO;
    // Paired-device lookup can initialize asynchronous framework work even
    // when the remote device already reports connected. Always offer that
    // work a default-mode processing slice before issuing SDP or RFCOMM.
    // The run loop may return early, and a callback may overrun the requested
    // timeout; the post-check and the parent's deadline remain authoritative.
    double requested = remaining < 0.05 ? remaining : 0.05;
    SInt32 result = runtime->pump(requested);
    bt_trace("startup event processing result=%d", (int)result);
    return runtime->now() < deadline;
}

static void *parent_liveness_watchdog(void *argument) {
    int fd = (int)(intptr_t)argument;
    uint8_t byte;
    for (;;) {
        ssize_t count = read(fd, &byte, sizeof(byte));
        if (count > 0) continue;
        if (count < 0 && errno == EINTR) continue;
        // EOF proves every copy of the parent's write endpoint is gone. Use
        // _exit because the helper main thread may be wedged in IOBluetooth
        // and no Objective-C or stdio cleanup is safe from this watchdog.
        _exit(83);
    }
}

static int start_parent_liveness_watchdog(int fd) {
    if (fd < 0 || fcntl(fd, F_GETFD) < 0) return -1;
    pthread_t thread;
    int result = pthread_create(
        &thread, NULL, parent_liveness_watchdog, (void *)(intptr_t)fd
    );
    if (result != 0) {
        errno = result;
        return -1;
    }
    result = pthread_detach(thread);
    if (result != 0) {
        errno = result;
        return -1;
    }
    return 0;
}

@implementation RfcommDelegate
- (void)rfcommChannelOpenComplete:(IOBluetoothRFCOMMChannel *)channel
                           status:(IOReturn)status {
    (void)channel;
    bt_trace("RFCOMM open complete status=0x%08x", (unsigned)status);
    pthread_mutex_lock(&g_context_mutex);
    if (_ctx) {
        if (status == kIOReturnSuccess && _ctx->state == 0 && !_ctx->close_observed) {
            _ctx->state = 1;
        } else {
            _ctx->state = -1;
        }
    }
    pthread_mutex_unlock(&g_context_mutex);
}

- (void)rfcommChannelData:(IOBluetoothRFCOMMChannel *)channel
                     data:(void *)data
                   length:(size_t)length {
    (void)channel;
    pthread_mutex_lock(&g_context_mutex);
    if (_ctx) {
        if (_ctx->output_fd >= 0) {
            // stdout deliberately remains blocking. If the parent stops
            // draining, only this disposable helper can stall; close/timeout
            // SIGKILLs it. Dropping or partially forwarding bytes here would
            // corrupt the CAT/MCP stream while pretending the link was healthy.
            if (write_all(_ctx->output_fd, data, length) != 0) {
                _ctx->state = -1;
            }
        } else if (_ctx->pre_ready_length <= BT_HELPER_PRE_READY_CAPACITY &&
                   length <=
                       BT_HELPER_PRE_READY_CAPACITY - _ctx->pre_ready_length) {
            // RFCOMM may deliver bytes in the same run-loop slice as its open
            // callback. Preserve them until READY is fully emitted so radio
            // ingress cannot corrupt the readiness handshake or disappear.
            memcpy(_ctx->pre_ready + _ctx->pre_ready_length, data, length);
            _ctx->pre_ready_length += length;
        } else {
            // Never pretend a stream is valid after losing ingress. The
            // helper exits and forces its parent to reopen from a clean link.
            _ctx->state = -1;
        }
    }
    pthread_mutex_unlock(&g_context_mutex);
}

- (void)rfcommChannelClosed:(IOBluetoothRFCOMMChannel *)channel {
    (void)channel;
    pthread_mutex_lock(&g_context_mutex);
    if (_ctx) {
        _ctx->state = 0;
        _ctx->close_observed = YES;
    }
    pthread_mutex_unlock(&g_context_mutex);
}
@end

static BOOL destroy_rfcomm_context_with_pump(RfcommContext *ctx,
                                            SInt32 (*pump)(double seconds)) {
    if (!ctx) return YES;

    // A healthy helper owns the channel until close completion. Pump the
    // helper run loop here so the SPP session is released before another
    // process attempts to open it. The Rust parent bounds the whole helper and
    // can still SIGKILL a framework call that does not return.
    pthread_mutex_lock(&g_context_mutex);
    ctx->output_fd = -1;
    pthread_mutex_unlock(&g_context_mutex);
    BOOL close_completed = !ctx->channel || ctx->close_observed;
    if (!close_completed) {
        bt_trace("RFCOMM close begin");
        [ctx->channel closeChannel];
        for (int attempt = 0; attempt < 50 && !ctx->close_observed; attempt++) {
            pump(0.01);
        }
        close_completed = ctx->close_observed;
        bt_trace("RFCOMM close end state=%d confirmed=%d", ctx->state, close_completed);
    }

    pthread_mutex_lock(&g_context_mutex);
    ctx->delegate.ctx = NULL;
    ctx->state = -1;
    pthread_mutex_unlock(&g_context_mutex);

    BOOL delegate_detached = YES;
    if (ctx->channel) {
        IOReturn detached = [ctx->channel setDelegate:nil];
        delegate_detached = detached == kIOReturnSuccess;
        if (!delegate_detached) {
            bt_trace("RFCOMM delegate detach failed status=0x%08x", (unsigned)detached);
        }
    }
    if (!close_completed || !delegate_detached) {
        g_unconfirmed_close = ctx;
        return NO;
    }
    ctx->channel = nil;
    ctx->delegate = nil;
    ctx->device = nil;
    free(ctx);
    return close_completed;
}

static BOOL destroy_rfcomm_context(RfcommContext *ctx) {
    // Production cleanup always uses the real run-loop pump; only no-radio
    // fixtures substitute a counted, nonblocking event processor.
    return destroy_rfcomm_context_with_pump(ctx, pump_open_events);
}

static RfcommContext *fail_open(RfcommContext *ctx, int stage, int *failure,
                               const NativeOpenRuntime *runtime) {
    *failure = stage;
    destroy_rfcomm_context_with_pump(ctx, runtime->pump);
    return NULL;
}

static int sdp_phase_failure(BOOL connected, BOOL requires_callback,
                             int callback_state, BOOL expired) {
    if (requires_callback && callback_state < 0) return BT_HELPER_EXIT_SDP_COMPLETION;
    if (expired) return BT_HELPER_EXIT_SDP_DEADLINE;
    if (!connected || (requires_callback && callback_state != 1)) {
        return BT_HELPER_EXIT_SDP_COMPLETION;
    }
    return 0;
}

static int rfcomm_phase_failure(int state, BOOL closed, BOOL expired,
                                BOOL endpoint_matches) {
    if (state < 0 || closed) return BT_HELPER_EXIT_RFCOMM_COMPLETION;
    if (expired) return BT_HELPER_EXIT_RFCOMM_DEADLINE;
    if (state != 1) return BT_HELPER_EXIT_RFCOMM_COMPLETION;
    if (!endpoint_matches) return BT_HELPER_EXIT_RFCOMM_ENDPOINT;
    return 0;
}

static int test_open_stage_classification(void) {
    // A connected device or cached service list alone does not complete a
    // callback-required query.
    if (sdp_phase_failure(YES, YES, 0, YES) != BT_HELPER_EXIT_SDP_DEADLINE) return 99;
    if (sdp_phase_failure(NO, NO, 0, YES) != BT_HELPER_EXIT_SDP_DEADLINE) return 99;
    if (sdp_phase_failure(YES, YES, -1, NO) != BT_HELPER_EXIT_SDP_COMPLETION) return 99;
    if (sdp_phase_failure(NO, YES, 1, NO) != BT_HELPER_EXIT_SDP_COMPLETION) return 99;
    if (sdp_phase_failure(YES, YES, 1, NO) != 0) return 99;
    if (sdp_phase_failure(YES, NO, 0, NO) != 0) return 99;
    if (rfcomm_phase_failure(0, NO, YES, YES) != BT_HELPER_EXIT_RFCOMM_DEADLINE) return 99;
    if (rfcomm_phase_failure(-1, NO, NO, YES) != BT_HELPER_EXIT_RFCOMM_COMPLETION) return 99;
    if (rfcomm_phase_failure(0, YES, NO, YES) != BT_HELPER_EXIT_RFCOMM_COMPLETION) return 99;
    if (rfcomm_phase_failure(1, NO, NO, NO) != BT_HELPER_EXIT_RFCOMM_ENDPOINT) return 99;
    if (rfcomm_phase_failure(1, NO, NO, YES) != 0) return 99;
    return 0;
}

// Keep the Objective-C out-parameter and context handoff in one place so no
// failed-open branch can leave a pending channel outside `ctx`, which closes it.
static IOReturn begin_rfcomm_channel(id device, RfcommContext *ctx,
                                    BluetoothRFCOMMChannelID channel_id) {
    IOBluetoothRFCOMMChannel *channel = nil;
    IOReturn result = [device openRFCOMMChannelAsync:&channel
                                     withChannelID:channel_id
                                          delegate:ctx->delegate];
    // The imported Objective-C out parameter is autoreleasing. ARC performs
    // writeback into the strong local; the context then retains that object.
    // A bridge to CF does not create a separately owned reference to consume.
    // Do not add CFBridgingRelease/CFRelease based on the legacy retained-out
    // prose: an additional release can invalidate a pending autorelease entry.
    ctx->channel = channel;
    return result;
}

// No-radio cleanup fixture. The fake uses the SDK's imported autoreleasing
// out-parameter convention, with ordinary Objective-C objects and no device
// discovery, SDP request or RFCOMM operation. An early deallocation exits
// before a stale autorelease entry could be consumed by the test process.
static BOOL g_cleanup_test_may_deallocate = NO;
static unsigned g_cleanup_test_deallocations = 0;
static unsigned g_cleanup_test_closes = 0;
static unsigned g_cleanup_test_pumps = 0;
static BOOL g_cleanup_test_invalid_pump = NO;
static BOOL g_cleanup_test_deliver_close = YES;
static BOOL g_cleanup_test_detach_delegate = YES;

// Ownership assertions must not depend on real run-loop scheduling. Exercise
// the same cleanup loop, counting its exact requested slices without waiting.
static SInt32 cleanup_test_pump(double seconds) {
    g_cleanup_test_pumps++;
    g_cleanup_test_invalid_pump |= seconds != 0.01 || g_cleanup_test_closes != 1;
    return kCFRunLoopRunTimedOut;
}

@interface CleanupTestChannel : NSObject {
    __weak id _delegate;
}
- (IOReturn)setDelegate:(id)delegate;
- (IOReturn)closeChannel;
@end

@implementation CleanupTestChannel
- (IOReturn)setDelegate:(id)delegate {
    if (!delegate && !g_cleanup_test_detach_delegate) return kIOReturnError;
    _delegate = delegate;
    return kIOReturnSuccess;
}
- (IOReturn)closeChannel {
    g_cleanup_test_closes++;
    if (g_cleanup_test_deliver_close) {
        [_delegate rfcommChannelClosed:(IOBluetoothRFCOMMChannel *)self];
    }
    return kIOReturnSuccess;
}
- (void)dealloc {
    if (!g_cleanup_test_may_deallocate) _exit(93);
    g_cleanup_test_deallocations++;
}
@end

@interface CleanupTestDevice : NSObject
- (IOReturn)openRFCOMMChannelAsync:(IOBluetoothRFCOMMChannel **)channel
                   withChannelID:(BluetoothRFCOMMChannelID)channel_id
                        delegate:(id)delegate;
@end

@implementation CleanupTestDevice
- (IOReturn)openRFCOMMChannelAsync:(IOBluetoothRFCOMMChannel **)channel
                   withChannelID:(BluetoothRFCOMMChannelID)channel_id
                        delegate:(id)delegate {
    (void)channel_id;
    CleanupTestChannel *created = [[CleanupTestChannel alloc] init];
    [created setDelegate:delegate];
    *channel = (IOBluetoothRFCOMMChannel *)created;
    return kIOReturnSuccess;
}
@end

static int test_pending_open_cleanup(BOOL deliver_close, BOOL detach_delegate) {
    g_cleanup_test_may_deallocate = NO;
    g_cleanup_test_deallocations = 0;
    g_cleanup_test_closes = 0;
    g_cleanup_test_pumps = 0;
    g_cleanup_test_invalid_pump = NO;
    g_cleanup_test_deliver_close = deliver_close;
    g_cleanup_test_detach_delegate = detach_delegate;
    BOOL closed = NO;
    __weak id observed_channel = nil;
    __weak id observed_delegate = nil;
    @autoreleasepool {
        RfcommContext *ctx = calloc(1, sizeof(RfcommContext));
        if (!ctx) return 94;
        ctx->output_fd = -1;
        ctx->delegate = [[RfcommDelegate alloc] init];
        ctx->delegate.ctx = ctx;
        CleanupTestDevice *device = [[CleanupTestDevice alloc] init];
        if (begin_rfcomm_channel(device, ctx, 2) != kIOReturnSuccess) return 95;
        observed_channel = ctx->channel;
        observed_delegate = ctx->delegate;
        closed = destroy_rfcomm_context_with_pump(ctx, cleanup_test_pump);
        g_cleanup_test_may_deallocate = YES;
    }
    if (g_cleanup_test_invalid_pump ||
        g_cleanup_test_pumps != (deliver_close ? 0 : 50)) return 98;
    if (!deliver_close || !detach_delegate) {
        if (g_unconfirmed_close) {
            // A queued late callback still targets a live delegate, but its
            // `ctx` link is NULL, so it cannot reach the retired context.
            [g_unconfirmed_close->delegate
                rfcommChannelOpenComplete:g_unconfirmed_close->channel
                status:kIOReturnSuccess];
        }
        return !closed && g_cleanup_test_closes == 1 &&
            g_cleanup_test_deallocations == 0 && g_unconfirmed_close &&
            observed_channel && observed_delegate &&
            g_unconfirmed_close->delegate.ctx == NULL ? 0 : 97;
    }
    return closed && g_cleanup_test_closes == 1 &&
        g_cleanup_test_deallocations == 1 ? 0 : 96;
}

static int test_closed_before_open(void) {
    RfcommContext *ctx = calloc(1, sizeof(RfcommContext));
    if (!ctx) return 94;
    ctx->output_fd = -1;
    ctx->delegate = [[RfcommDelegate alloc] init];
    ctx->delegate.ctx = ctx;
    [ctx->delegate rfcommChannelClosed:nil];
    [ctx->delegate rfcommChannelOpenComplete:nil status:kIOReturnSuccess];
    BOOL refused = ctx->state == -1 && ctx->close_observed;
    destroy_rfcomm_context(ctx);
    return refused ? 0 : 98;
}

static int is_ascii_hex_digit(char byte) {
    return (byte >= '0' && byte <= '9') ||
           (byte >= 'A' && byte <= 'F') ||
           (byte >= 'a' && byte <= 'f');
}

static int device_identifier_is_exact_address(const char *identifier) {
    if (!identifier || strlen(identifier) != 17) return 0;
    char separator = identifier[2];
    if (separator != '-' && separator != ':') return 0;
    for (size_t index = 0; index < 17; index++) {
        if ((index + 1) % 3 == 0) {
            if (identifier[index] != separator) return 0;
        } else if (!is_ascii_hex_digit(identifier[index])) {
            return 0;
        }
    }
    return 1;
}

// Shared by production selection and the Rust regression test. An address is
// identity-bearing even when another paired device was given that exact text
// as its display name, so it must never enter the name-matching path.
int bt_device_identifier_matches_display_name(const char *identifier,
                                               const char *display_name) {
    if (!identifier || !display_name ||
        device_identifier_is_exact_address(identifier)) {
        return 0;
    }
    return strcmp(identifier, display_name) == 0;
}

// Runs only in the helper process's main thread, which owns and pumps the
// CFRunLoop used for all IOBluetooth callbacks.
static RfcommContext *open_selected_device(IOBluetoothDevice *device,
                                           uint8_t rfcomm_channel,
                                           BOOL resolve_serial_port,
                                           int *failure,
                                           double open_deadline,
                                           const NativeOpenRuntime *runtime);

static RfcommContext *open_rfcomm(const char *device_identifier,
                                  uint8_t rfcomm_channel,
                                  BOOL resolve_serial_port,
                                  int *failure) {
    @autoreleasepool {
        *failure = 71;
        NSString *identifier = [NSString
            stringWithUTF8String:device_identifier];
        if (!identifier) {
            return fail_open(NULL, BT_HELPER_EXIT_CONTEXT_ALLOCATION,
                             failure, &kNativeOpenRuntime);
        }
        int exact_address_selector =
            device_identifier_is_exact_address(device_identifier);
        IOBluetoothDevice *device = nil;
        IOBluetoothDevice *name_match = nil;
        NSUInteger name_match_count = 0;
        for (IOBluetoothDevice *paired_device in
             [IOBluetoothDevice pairedDevices]) {
            // An exact address is globally identifying and takes precedence
            // even when an earlier device happened to share the same name.
            NSString *paired_address = paired_device.addressString;
            if (paired_address &&
                [paired_address caseInsensitiveCompare:identifier]
                    == NSOrderedSame) {
                device = paired_device;
                break;
            }
            if (bt_device_identifier_matches_display_name(
                    device_identifier, paired_device.name.UTF8String)) {
                name_match = paired_device;
                name_match_count++;
            }
        }
        // An exact-address request is identity-bearing. If that paired
        // address disappeared, never reinterpret the same bytes as a display
        // name and silently open a different device.
        if (!device && exact_address_selector) return NULL;
        if (!device && name_match_count > 1) {
            *failure = BT_HELPER_EXIT_AMBIGUOUS_DEVICE_NAME;
            bt_trace("paired device name is ambiguous name=%s matches=%lu",
                     device_identifier, (unsigned long)name_match_count);
            return NULL;
        }
        if (!device) device = name_match;
        if (!device) return NULL;

        return open_selected_device(device, rfcomm_channel, resolve_serial_port,
            failure, monotonic_seconds() + 20.0, &kNativeOpenRuntime);
    }
}

static RfcommContext *open_selected_device(IOBluetoothDevice *device,
                                           uint8_t rfcomm_channel,
                                           BOOL resolve_serial_port,
                                           int *failure,
                                           double open_deadline,
                                           const NativeOpenRuntime *runtime) {
    @autoreleasepool {
        if (!process_startup_events(open_deadline, runtime)) {
            return fail_open(NULL, BT_HELPER_EXIT_STARTUP_DEADLINE, failure, runtime);
        }
        RfcommContext *ctx = calloc(1, sizeof(RfcommContext));
        if (!ctx) return fail_open(NULL, BT_HELPER_EXIT_CONTEXT_ALLOCATION, failure, runtime);
        ctx->state = 0;
        ctx->output_fd = -1;
        ctx->device = device;
        ctx->delegate = [[RfcommDelegate alloc] init];
        if (!ctx->delegate) return fail_open(ctx, BT_HELPER_EXIT_CONTEXT_ALLOCATION, failure, runtime);
        ctx->delegate.ctx = ctx;

        // A connected baseband is valid and may be shared by other Bluetooth
        // profiles. Never tear it down as an RFCOMM cleanup step.
        // Fixed-channel callers retain the established nil-target baseband
        // wakeup. SerialPort callers instead require successful completion of
        // this helper's fresh SDP query before using any service record.
        // Startup processing, both branches and RFCOMM opening share one
        // native deadline.
        SdpQueryDelegate *sdp = resolve_serial_port ? [[SdpQueryDelegate alloc] init] : nil;
        if (resolve_serial_port && !sdp) {
            return fail_open(ctx, BT_HELPER_EXIT_CONTEXT_ALLOCATION, failure, runtime);
        }
        if (sdp) {
            sdp->device = device;
            sdp->state = 0;
            g_pending_sdp = sdp;
        }
        if (runtime->now() >= open_deadline) {
            return fail_open(ctx, BT_HELPER_EXIT_STARTUP_DEADLINE, failure, runtime);
        }
        IOReturn query_result = [device performSDPQuery:sdp];
        bt_trace("SDP start status=0x%08x connected=%d",
                 (unsigned)query_result, [device isConnected]);
        if (query_result != kIOReturnSuccess) {
            return fail_open(ctx, BT_HELPER_EXIT_SDP_START, failure, runtime);
        }
        while (resolve_serial_port ? sdp->state == 0 : ![device isConnected]) {
            double remaining = open_deadline - runtime->now();
            if (remaining <= 0.0) break;
            runtime->pump(remaining < 0.05 ? remaining : 0.05);
        }
        int sdp_failure = sdp_phase_failure([device isConnected], resolve_serial_port,
            sdp ? sdp->state : 0, runtime->now() >= open_deadline);
        if (sdp_failure != 0) {
            bt_trace("SDP failed stage=%d connected=%d", sdp_failure, [device isConnected]);
            return fail_open(ctx, sdp_failure, failure, runtime);
        }

        if (resolve_serial_port) {
            NSUInteger matches = 0;
            for (IOBluetoothSDPServiceRecord *record in device.services) {
                if (![record matchesUUID16:0x1101]) continue;
                BluetoothRFCOMMChannelID resolved = 0;
                if ([record getRFCOMMChannelID:&resolved] != kIOReturnSuccess ||
                    resolved < 1 || resolved > 30) {
                    return fail_open(ctx, BT_HELPER_EXIT_SERVICE_RESOLUTION, failure, runtime);
                }
                rfcomm_channel = resolved;
                matches++;
            }
            if (matches != 1) {
                bt_trace("Serial Port SDP requires one record, got %lu", (unsigned long)matches);
                return fail_open(ctx, BT_HELPER_EXIT_SERVICE_RESOLUTION, failure, runtime);
            }
        }
        if (runtime->now() >= open_deadline) {
            return fail_open(ctx, BT_HELPER_EXIT_SDP_DEADLINE, failure, runtime);
        }

        bt_trace("RFCOMM open requested device=%s channel=%u",
                 device.addressString.UTF8String, rfcomm_channel);
        if (runtime->now() >= open_deadline) {
            return fail_open(ctx, BT_HELPER_EXIT_SDP_DEADLINE, failure, runtime);
        }
        IOReturn result = begin_rfcomm_channel(device, ctx, rfcomm_channel);
        if (result != kIOReturnSuccess) {
            bt_trace("RFCOMM open start failed status=0x%08x",
                     (unsigned)result);
            return fail_open(ctx, BT_HELPER_EXIT_RFCOMM_START, failure, runtime);
        }

        while (ctx->state == 0) {
            double remaining = open_deadline - runtime->now();
            if (remaining <= 0.0) break;
            runtime->pump(remaining < 0.05 ? remaining : 0.05);
        }
        int rfcomm_failure = rfcomm_phase_failure(ctx->state, ctx->close_observed,
            runtime->now() >= open_deadline,
            ctx->channel && [ctx->channel getChannelID] == rfcomm_channel);
        if (rfcomm_failure != 0) {
            bt_trace("RFCOMM open failed stage=%d state=%d", rfcomm_failure, ctx->state);
            return fail_open(ctx, rfcomm_failure, failure, runtime);
        }

        return ctx;
    }
}

static int write_all(int fd, const uint8_t *bytes, size_t length) {
    size_t offset = 0;
    while (offset < length) {
        ssize_t count = write(fd, bytes + offset, length - offset);
        if (count > 0) {
            offset += (size_t)count;
            continue;
        }
        if (count < 0 && errno == EINTR) continue;
        return -1;
    }
    return 0;
}

static int report_failed_open(int failure) {
    if (failure >= BT_HELPER_EXIT_CONTEXT_ALLOCATION &&
        failure <= BT_HELPER_EXIT_STARTUP_DEADLINE) {
        const uint8_t record[] = {
            (uint8_t)failure, g_unconfirmed_close ? 1 : 0,
        };
        if (write_all(STDOUT_FILENO, kOpenFailureMagic, sizeof(kOpenFailureMagic) - 1) != 0 ||
            write_all(STDOUT_FILENO, record, sizeof(record)) != 0) return 72;
    }
    return g_unconfirmed_close ? BT_HELPER_EXIT_CLOSE_UNCONFIRMED : failure;
}

static int run_helper(const char *device_name, uint8_t channel_id, BOOL resolve_serial_port) {
    int failure = 71;
    RfcommContext *ctx = open_rfcomm(
        device_name, channel_id, resolve_serial_port, &failure
    );
    if (!ctx) {
        bt_trace("native open failed device=%s stage=%d", device_name, failure);
        return report_failed_open(failure);
    }

    BluetoothRFCOMMMTU mtu = [ctx->channel getMTU];
    if (mtu == 0) {
        destroy_rfcomm_context(ctx);
        return 74;
    }

    // READY and its fixed endpoint record precede all radio data. Ingress during
    // open was retained in the bounded pre-ready buffer; flush it only after
    // the complete prefix so the parent can consume an unambiguous handshake.
    if (write_all(STDOUT_FILENO, kReadyMagic, sizeof(kReadyMagic) - 1) != 0) {
        destroy_rfcomm_context(ctx);
        return 72;
    }
    const char *actual_address = ctx->device.addressString.UTF8String;
    uint8_t actual_channel = [ctx->channel getChannelID];
    if (!device_identifier_is_exact_address(actual_address) ||
        actual_channel < 1 || actual_channel > 30 ||
        write_all(STDOUT_FILENO, (const uint8_t *)actual_address, 17) != 0 ||
        write_all(STDOUT_FILENO, &actual_channel, 1) != 0) {
        destroy_rfcomm_context(ctx);
        return 72;
    }
    if (bt_fd_set_nonblocking(STDIN_FILENO) != 0) {
        destroy_rfcomm_context(ctx);
        return 73;
    }
    int ingress_result = 0;
    pthread_mutex_lock(&g_context_mutex);
    if (ctx->state != 1) {
        ingress_result = -1;
    } else if (ctx->pre_ready_length > 0 &&
               write_all(STDOUT_FILENO, ctx->pre_ready,
                         ctx->pre_ready_length) != 0) {
        ctx->state = -1;
        ingress_result = -1;
    } else {
        ctx->pre_ready_length = 0;
        ctx->output_fd = STDOUT_FILENO;
    }
    pthread_mutex_unlock(&g_context_mutex);
    if (ingress_result != 0) {
        destroy_rfcomm_context(ctx);
        return 84;
    }

    bt_trace("ready device=%s channel=%u mtu=%u",
             actual_address, actual_channel, (unsigned)mtu);

    // PIPE_BUF is 512 bytes on macOS. The parent submits at most that much
    // per atomic pipe write; cap reads to the smaller of PIPE_BUF and the
    // negotiated RFCOMM MTU. writeSync: may wedge indefinitely, which is
    // safe here because this entire process is the cancellation boundary.
    uint8_t bytes[512];
    size_t read_limit = (size_t)mtu < sizeof(bytes)
        ? (size_t)mtu : sizeof(bytes);
    int exit_code = 0;
    while (ctx->state == 1) {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, false);

        ssize_t count = read(STDIN_FILENO, bytes, read_limit);
        if (count > 0) {
            bt_trace("writeSync enter bytes=%zd", count);
            IOReturn result = [ctx->channel writeSync:bytes
                                                  length:(UInt16)count];
            bt_trace("writeSync exit status=0x%08x", (unsigned)result);
            if (result != kIOReturnSuccess) {
                exit_code = 75;
                break;
            }
            continue;
        }
        if (count == 0) break;
        if (errno == EINTR) continue;
        if (errno != EAGAIN && errno != EWOULDBLOCK) {
            exit_code = 76;
            break;
        }
        usleep(1000);
    }

    BOOL closed = destroy_rfcomm_context(ctx);
    if (exit_code == 0 && !closed) return BT_HELPER_EXIT_CLOSE_UNCONFIRMED;
    return exit_code;
}

// Returns 0 after writing one record, 1 when the framework record is unusable,
// and -1 on stdout failure.
static int write_paired_device_record(IOBluetoothDevice *device) {
    NSData *address = [[device addressString]
        dataUsingEncoding:NSUTF8StringEncoding];
    NSString *display_name = device.name ?: device.addressString;
    NSData *name = [display_name dataUsingEncoding:NSUTF8StringEncoding];
    if (!address || !name || address.length == 0 ||
        address.length > UINT16_MAX || name.length == 0 ||
        name.length > UINT16_MAX) {
        return 1;
    }
    uint8_t header[4] = {
        (uint8_t)(address.length >> 8),
        (uint8_t)address.length,
        (uint8_t)(name.length >> 8),
        (uint8_t)name.length,
    };
    if (write_all(STDOUT_FILENO, header, sizeof(header)) != 0 ||
        write_all(STDOUT_FILENO, address.bytes, address.length) != 0 ||
        write_all(STDOUT_FILENO, name.bytes, name.length) != 0) {
        return -1;
    }
    return 0;
}

// Production helper control modes use the same early constructor as the raw
// radio stream. Enumeration returns every bounded paired device using only its
// exact address and display name. Radio identity is established later, after
// the user or same-radio recovery policy chooses an exact address.
static int run_control_helper(const char *mode) {
    if (write_all(STDOUT_FILENO, kReadyMagic, sizeof(kReadyMagic) - 1) != 0) {
        return 79;
    }
    if (strcmp(mode, "paired") == 0) {
        @autoreleasepool {
            NSArray *devices = [IOBluetoothDevice pairedDevices];
            NSUInteger emitted = 0;
            for (IOBluetoothDevice *device in devices) {
                if (emitted >= BT_HELPER_MAX_PAIRED_DEVICES) {
                    return BT_HELPER_EXIT_TOO_MANY_PAIRED_DEVICES;
                }
                int result = write_paired_device_record(device);
                if (result < 0) return 86;
                if (result == 0) emitted++;
            }
            const uint8_t terminator[4] = {0, 0, 0, 0};
            return write_all(STDOUT_FILENO, terminator,
                             sizeof(terminator)) == 0 ? 0 : 86;
        }
    }
    return 82;
}

// No-radio lifecycle probes. The exact helper sentinel is still required, so
// an ambient test-mode variable alone has no effect.
#include "bluetooth_startup_tests.m"
#include "bluetooth_open_failure_tests.m"

static int run_test_helper(const char *mode) {
    if (strcmp(mode, "open-failure-cleanup-v1") == 0) {
        return test_open_failure_cleanup(NO);
    }
    if (strcmp(mode, "open-failure-clean-v1") == 0) {
        return test_open_failure_cleanup(YES);
    }
    if (write_all(STDOUT_FILENO, kReadyMagic, sizeof(kReadyMagic) - 1) != 0) {
        return 79;
    }
    if (strcmp(mode, "echo-v1") == 0) {
        uint8_t bytes[512];
        for (;;) {
            ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
            if (count > 0) {
                if (write_all(STDOUT_FILENO, bytes, (size_t)count) != 0) {
                    return 80;
                }
                continue;
            }
            if (count == 0) return 0;
            if (errno != EINTR) return 81;
        }
    }
    if (strcmp(mode, "hang-v1") == 0) {
        for (;;) pause();
    }
    if (strcmp(mode, "duplicate-sdp-v1") == 0) {
        // Exercise the real callback's duplicate guard without a device,
        // SDP query, enumeration, or RFCOMM open.
        SdpQueryDelegate *target = [[SdpQueryDelegate alloc] init];
        target->device = nil;
        target->state = 0;
        [target sdpQueryComplete:nil status:kIOReturnSuccess];
        [target sdpQueryComplete:nil status:kIOReturnSuccess];
        return 91;
    }
    if (strcmp(mode, "close-output-v1") == 0) {
        // A final native callback may emit ingress after parent stdin EOF.
        // The parent must retain stdout while it waits for orderly teardown.
        uint8_t byte;
        ssize_t count;
        do {
            count = read(STDIN_FILENO, &byte, 1);
        } while (count > 0 || (count < 0 && errno == EINTR));
        const uint8_t final_byte = 0x55;
        return count == 0 && write_all(STDOUT_FILENO, &final_byte, 1) == 0 ? 0 : 92;
    }
    if (strcmp(mode, "pending-open-cleanup-v1") == 0) {
        return test_pending_open_cleanup(YES, YES);
    }
    if (strcmp(mode, "unconfirmed-open-cleanup-v1") == 0) {
        return test_pending_open_cleanup(NO, YES);
    }
    if (strcmp(mode, "failed-detach-cleanup-v1") == 0) {
        return test_pending_open_cleanup(YES, NO);
    }
    if (strcmp(mode, "closed-before-open-v1") == 0) {
        return test_closed_before_open();
    }
    if (strcmp(mode, "open-stage-classification-v1") == 0) {
        return test_open_stage_classification();
    }
    if (strcmp(mode, "startup-progress-v1") == 0) {
        return test_startup_progress();
    }
    return 82;
}

__attribute__((constructor))
static void bluetooth_helper_constructor(void) {
    const char *sentinel = getenv(BT_HELPER_SENTINEL_ENV);
    if (!sentinel || strcmp(sentinel, BT_HELPER_SENTINEL_VALUE) != 0) return;

    // A dead parent should yield EPIPE rather than terminating in a signal
    // while the helper is unwinding its channel.
    signal(SIGPIPE, SIG_IGN);
    // Interactive clients own Ctrl-C/Ctrl-\ and may use them to leave a
    // monitor without ending the radio connection. The helper shares the
    // parent's foreground process group, so it must not inherit the default
    // terminal-signal actions. Parent-liveness EOF and explicit TERM/KILL
    // cleanup remain authoritative.
    signal(SIGINT, SIG_IGN);
    signal(SIGQUIT, SIG_IGN);

    const char *liveness_env = getenv(BT_HELPER_LIVENESS_FD_ENV);
    if (!liveness_env || !liveness_env[0]) _exit(85);
    errno = 0;
    char *liveness_end = NULL;
    long parsed_liveness = strtol(liveness_env, &liveness_end, 10);
    if (errno == ERANGE || !liveness_end || liveness_end == liveness_env ||
        *liveness_end != '\0' || parsed_liveness < 0 ||
        parsed_liveness > INT_MAX ||
        start_parent_liveness_watchdog((int)parsed_liveness) != 0) {
        _exit(85);
    }

    const char *control_mode = getenv(BT_HELPER_CONTROL_ENV);
    if (control_mode && control_mode[0]) _exit(run_control_helper(control_mode));

    const char *test_mode = getenv(BT_HELPER_TEST_ENV);
    if (test_mode && test_mode[0]) _exit(run_test_helper(test_mode));

    const char *device_env = getenv(BT_HELPER_DEVICE_ENV);
    const char *channel_env = getenv(BT_HELPER_CHANNEL_ENV);
    if (!device_env || !device_env[0] || !channel_env) _exit(77);

    BOOL resolve_serial_port = strcmp(channel_env, "spp") == 0;
    long parsed_channel = 0;
    if (!resolve_serial_port) {
        char *end = NULL;
        errno = 0;
        parsed_channel = strtol(channel_env, &end, 10);
        if (errno == ERANGE || !end || end == channel_env || *end != '\0' ||
            parsed_channel < 1 || parsed_channel > 30) _exit(78);
    }

    int result = run_helper(device_env, (uint8_t)parsed_channel, resolve_serial_port);
    _exit(result);
}

#endif
