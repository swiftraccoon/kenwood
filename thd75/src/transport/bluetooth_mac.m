#ifdef __APPLE__

#import <Foundation/Foundation.h>
#import <IOBluetooth/IOBluetooth.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <sys/time.h>
#include <unistd.h>

// Opt-in shim tracing. Set THD75_BT_TRACE=1 before launching the
// host (thd75-repl/-tui/examples) to get microsecond-stamped entry
// and exit lines for every FFI boundary that can block the main
// thread: bt_pump_runloop, bt_rfcomm_write's writeSync:, and the
// rfcommChannelData: delegate callback (whose blocking write()
// into the ingress pipe is the most plausible freeze point).
//
// Written to stderr (unbuffered via fflush) so the output shows up
// alongside Rust tracing without requiring a new sink. Wall-clock
// timestamps let you line these up with the thd75-repl trace log.
static _Atomic int g_bt_trace = -1;

static int bt_trace_enabled(void) {
    int v = g_bt_trace;
    if (v < 0) {
        const char *e = getenv("THD75_BT_TRACE");
        v = (e && e[0] && e[0] != '0') ? 1 : 0;
        g_bt_trace = v;
    }
    return v;
}

// Print a microsecond-stamped line to stderr. Uses vfprintf so the
// caller site looks like a regular printf, but with the timestamp
// and a "[bt] " prefix auto-prepended. NO-OP when tracing is off.
static void bt_trace(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void bt_trace(const char *fmt, ...) {
    if (!bt_trace_enabled()) return;
    struct timeval tv;
    gettimeofday(&tv, NULL);
    struct tm t;
    gmtime_r(&tv.tv_sec, &t);
    fprintf(stderr,
            "[bt] %02d:%02d:%02d.%06d thread=%s ",
            t.tm_hour, t.tm_min, t.tm_sec, (int)tv.tv_usec,
            [NSThread isMainThread] ? "main" : "other");
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
    fflush(stderr);
}

@class RfcommDelegate;

typedef struct {
    IOBluetoothDevice *device;
    IOBluetoothRFCOMMChannel *channel;
    RfcommDelegate *delegate;
    int pipe_read;
    int pipe_write;
    _Atomic int state;
} RfcommContext;

@interface RfcommDelegate : NSObject <IOBluetoothRFCOMMChannelDelegate>
@property (nonatomic, assign) RfcommContext *ctx;
@end

@implementation RfcommDelegate
- (void)rfcommChannelOpenComplete:(IOBluetoothRFCOMMChannel *)ch status:(IOReturn)e {
    if (_ctx && e == kIOReturnSuccess) _ctx->state = 1;
}
- (void)rfcommChannelData:(IOBluetoothRFCOMMChannel *)ch data:(void *)data length:(size_t)len {
    // This runs on the main thread's CFRunLoop. The write() below
    // can block if the ingress pipe fills (Rust side not draining
    // fast enough), and while it blocks the whole main thread is
    // wedged — nothing else on the LocalSet runs. Trace both sides
    // so a hang here is visible as "enter" with no matching "exit".
    bt_trace("rfcommChannelData enter len=%zu", len);
    if (_ctx && _ctx->pipe_write >= 0) {
        ssize_t w = write(_ctx->pipe_write, data, len);
        bt_trace("rfcommChannelData exit wrote=%zd", w);
    } else {
        bt_trace("rfcommChannelData exit no-pipe");
    }
}
- (void)rfcommChannelClosed:(IOBluetoothRFCOMMChannel *)ch {
    if (_ctx) _ctx->state = 0;
}
@end

static pthread_t g_pump_thread;
static _Atomic int g_pump_running = 0;
static _Atomic int g_open_count = 0;
static pthread_mutex_t g_bt_mutex = PTHREAD_MUTEX_INITIALIZER;

static void *pump_main_runloop(void *arg) {
    (void)arg;
    CFRunLoopRef mainRL = CFRunLoopGetMain();
    while (g_pump_running) {
        CFRunLoopWakeUp(mainRL);
        usleep(10000);
    }
    return NULL;
}

void bt_pump_runloop(void) {
    // Must pump the MAIN thread's run loop — IOBluetooth delivers
    // RFCOMM callbacks there regardless of which thread calls this.
    if ([NSThread isMainThread]) {
        // Bounded pump: 1 ms cap. Should always return promptly.
        // If a long "enter" with no matching "exit" shows up in a
        // hang's tail, the freeze is inside a CFRunLoop callback
        // (most likely rfcommChannelData: — see its own trace).
        bt_trace("bt_pump_runloop enter main");
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, false);
        bt_trace("bt_pump_runloop exit main");
    } else {
        // From a non-main thread, wake the main run loop so it processes
        // pending IOBluetooth callbacks. The pump_main_runloop background
        // thread also does this, but an explicit wake ensures timely delivery.
        bt_trace("bt_pump_runloop wake-main from other");
        CFRunLoopWakeUp(CFRunLoopGetMain());
    }
}

// Internal open — must be called from a thread with an active CFRunLoop.
static void *do_rfcomm_open(const char *device_name, uint8_t rfcomm_channel) {
    @autoreleasepool {
        NSString *name = [NSString stringWithUTF8String:device_name];
        IOBluetoothDevice *device = nil;
        for (IOBluetoothDevice *d in [IOBluetoothDevice pairedDevices]) {
            if ([d.name isEqualToString:name]) { device = d; break; }
        }
        if (!device) return NULL;
        // fprintf(stderr, "BT: device found, connected=%d\n", [device isConnected]);

        RfcommContext *ctx = calloc(1, sizeof(RfcommContext));
        ctx->state = 0;
        ctx->device = device;

        int fds[2];
        if (pipe(fds) != 0) { free(ctx); return NULL; }
        ctx->pipe_read = fds[0];
        ctx->pipe_write = fds[1];
        fcntl(ctx->pipe_read, F_SETFL, fcntl(ctx->pipe_read, F_GETFL) | O_NONBLOCK);

        ctx->delegate = [[RfcommDelegate alloc] init];
        ctx->delegate.ctx = ctx;

        // Close any stale connection (e.g. from the broken serial port driver)
        // then reconnect fresh via SDP.
        if ([device isConnected]) {
            // fprintf(stderr, "BT: closing stale connection\n");
            [device closeConnection];
            for (int i = 0; i < 60 && [device isConnected]; i++)
                usleep(50000);
        }

        // SDP query triggers fresh baseband connection
        [device performSDPQuery:nil];
        for (int i = 0; i < 100 && ![device isConnected]; i++)
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, false);

        if (![device isConnected]) {
            close(ctx->pipe_read); close(ctx->pipe_write);
            free(ctx); return NULL;
        }

        IOBluetoothRFCOMMChannel *channel = nil;
        IOReturn ret = [device openRFCOMMChannelAsync:&channel
                                        withChannelID:rfcomm_channel
                                             delegate:ctx->delegate];
        if (ret != kIOReturnSuccess) {
            ctx->delegate.ctx = NULL;
            close(ctx->pipe_read); close(ctx->pipe_write);
            free(ctx); return NULL;
        }

        for (int i = 0; i < 200 && ctx->state == 0; i++)
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, false);

        if (ctx->state != 1) {
            ctx->delegate.ctx = NULL;
            if (channel) { [channel setDelegate:nil]; [channel closeChannel]; }
            close(ctx->pipe_read); close(ctx->pipe_write);
            free(ctx); return NULL;
        }

        ctx->channel = channel;
        return ctx;
    }
}


void *bt_rfcomm_open(const char *device_name, uint8_t rfcomm_channel) {
    pthread_mutex_lock(&g_bt_mutex);
    if (g_open_count == 0) {
        g_pump_running = 1;
        pthread_create(&g_pump_thread, NULL, pump_main_runloop, NULL);
    }
    g_open_count++;
    pthread_mutex_unlock(&g_bt_mutex);

    void *r = do_rfcomm_open(device_name, rfcomm_channel);
    if (!r) {
        pthread_mutex_lock(&g_bt_mutex);
        g_open_count--;
        if (g_open_count == 0) {
            g_pump_running = 0;
            pthread_mutex_unlock(&g_bt_mutex);
            pthread_join(g_pump_thread, NULL);
        } else {
            pthread_mutex_unlock(&g_bt_mutex);
        }
    }
    return r;
}

int bt_rfcomm_write(void *handle, const uint8_t *data, size_t len) {
    RfcommContext *ctx = (RfcommContext *)handle;
    if (!ctx || !ctx->channel || ctx->state != 1 || len > UINT16_MAX) return -1;
    // writeSync: blocks until the peer acknowledges the RFCOMM
    // frame (or the channel errors out). If the radio's RFCOMM
    // buffer is full and the firmware has stalled, this is where
    // the main thread parks indefinitely. An "enter" line with no
    // matching "exit" in the trace dump narrows the hang to here
    // rather than bt_pump_runloop.
    bt_trace("bt_rfcomm_write enter len=%zu", len);
    @autoreleasepool {
        IOReturn r = [ctx->channel writeSync:(void *)data
                                      length:(UInt16)(len & 0xFFFF)];
        bt_trace("bt_rfcomm_write exit ret=0x%08x", (unsigned)r);
        return (r == kIOReturnSuccess) ? 0 : -1;
    }
}

int bt_rfcomm_read_fd(void *handle) {
    return handle ? ((RfcommContext *)handle)->pipe_read : -1;
}

int bt_rfcomm_is_connected(void *handle) {
    return handle ? ((RfcommContext *)handle)->state : 0;
}

void bt_rfcomm_close(void *handle) {
    RfcommContext *ctx = (RfcommContext *)handle;
    if (!ctx) return;
    @autoreleasepool {
        // Nil the delegate FIRST to prevent use-after-free in IOBluetooth
        // callbacks. IOBluetooth delivers rfcommChannelData: asynchronously
        // on the main run loop — if we free ctx before niling the delegate,
        // a late callback would dereference freed memory.
        ctx->delegate.ctx = NULL;
        if (ctx->channel) {
            [ctx->channel setDelegate:nil];
            [ctx->channel closeChannel];
            ctx->channel = nil;
        }
        ctx->state = -1;
        if (ctx->pipe_write >= 0) { close(ctx->pipe_write); ctx->pipe_write = -1; }
        if (ctx->pipe_read >= 0) { close(ctx->pipe_read); ctx->pipe_read = -1; }
        free(ctx);
        // Only stop the pump thread when the last connection closes.
        pthread_mutex_lock(&g_bt_mutex);
        g_open_count--;
        if (g_open_count <= 0) {
            g_open_count = 0;
            g_pump_running = 0;
            pthread_mutex_unlock(&g_bt_mutex);
            pthread_join(g_pump_thread, NULL);
        } else {
            pthread_mutex_unlock(&g_bt_mutex);
        }
    }
}

#endif
