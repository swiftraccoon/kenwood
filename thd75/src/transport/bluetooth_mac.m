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

static pthread_t g_pump_thread;
static _Atomic int g_pump_running = 0;
static _Atomic int g_open_count = 0;
static pthread_mutex_t g_bt_mutex = PTHREAD_MUTEX_INITIALIZER;
// Ingress bytes dropped because the (non-blocking) pipe was full,
// i.e. the Rust side stopped draining. Surfaces in traces; the CAT layer's
// timeout machinery handles the resulting gap.
static _Atomic unsigned long g_ingress_dropped = 0;

@implementation RfcommDelegate
- (void)rfcommChannelOpenComplete:(IOBluetoothRFCOMMChannel *)ch status:(IOReturn)e {
    // All _ctx access is serialized with bt_rfcomm_close under
    // g_bt_mutex: close() nulls _ctx under the mutex BEFORE freeing
    // the context, so a callback either sees a valid context (and
    // blocks close until it's done) or NULL, never freed memory.
    pthread_mutex_lock(&g_bt_mutex);
    if (_ctx && e == kIOReturnSuccess) _ctx->state = 1;
    pthread_mutex_unlock(&g_bt_mutex);
}
- (void)rfcommChannelData:(IOBluetoothRFCOMMChannel *)ch data:(void *)data length:(size_t)len {
    // Runs on the main thread's CFRunLoop. The pipe write end is
    // O_NONBLOCK: if the Rust side stops draining and the pipe
    // fills, bytes are DROPPED (and counted) instead of wedging the
    // entire main thread inside a CFRunLoop callback. The mutex hold
    // is therefore bounded (see rfcommChannelOpenComplete for the
    // close-synchronization contract).
    bt_trace("rfcommChannelData enter len=%zu", len);
    pthread_mutex_lock(&g_bt_mutex);
    if (_ctx && _ctx->pipe_write >= 0) {
        ssize_t w = write(_ctx->pipe_write, data, len);
        if (w < 0 || (size_t)w < len) {
            size_t dropped = (w < 0) ? len : (len - (size_t)w);
            unsigned long total = (g_ingress_dropped += dropped);
            fprintf(stderr,
                    "[bt] WARNING: ingress pipe full; dropped %zu bytes "
                    "(total %lu); consumer not draining\n",
                    dropped, total);
        }
        bt_trace("rfcommChannelData exit wrote=%zd", w);
    } else {
        bt_trace("rfcommChannelData exit no-pipe");
    }
    pthread_mutex_unlock(&g_bt_mutex);
}
- (void)rfcommChannelClosed:(IOBluetoothRFCOMMChannel *)ch {
    pthread_mutex_lock(&g_bt_mutex);
    if (_ctx) _ctx->state = 0;
    pthread_mutex_unlock(&g_bt_mutex);
}
@end

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
    // Must pump the MAIN thread's run loop: IOBluetooth delivers
    // RFCOMM callbacks there regardless of which thread calls this.
    if ([NSThread isMainThread]) {
        // Bounded pump: 1 ms cap. Should always return promptly.
        // If a long "enter" with no matching "exit" shows up in a
        // hang's tail, the freeze is inside a CFRunLoop callback
        // (most likely rfcommChannelData:, which has its own trace).
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

// Internal open: must be called from a thread with an active CFRunLoop.
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
        // Non-blocking WRITE end too: the delegate callback writes on
        // the main thread while holding g_bt_mutex, and a blocking write
        // into a full pipe would wedge the main thread (and deadlock
        // against close, which needs the same mutex). Overflow drops
        // are counted in g_ingress_dropped instead.
        fcntl(ctx->pipe_write, F_SETFL, fcntl(ctx->pipe_write, F_GETFL) | O_NONBLOCK);

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
    if (g_open_count > 0) {
        // Only one RFCOMM handle may exist per process: a second
        // open would call closeConnection on the live device (the
        // radio shows connected), tearing the baseband out from
        // under the first handle's channel: the documented SIGTRAP.
        // Refuse instead; the caller must close the first handle
        // fully before reconnecting.
        pthread_mutex_unlock(&g_bt_mutex);
        fprintf(stderr,
                "[bt] ERROR: refusing second RFCOMM open while a "
                "handle is still live; close it first\n");
        return NULL;
    }
    g_pump_running = 1;
    pthread_create(&g_pump_thread, NULL, pump_main_runloop, NULL);
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
    if (!ctx || len > UINT16_MAX) return -1;
    // writeSync: blocks until the peer acknowledges the RFCOMM frame
    // (or the channel errors out). The Rust side calls this from the
    // blocking pool, and its future may be dropped mid-write, so the
    // whole context access (including writeSync) runs under
    // g_bt_mutex: bt_rfcomm_close cannot free the context while a
    // write is in flight. A wedged write therefore delays close (and
    // any delegate callback) instead of causing a use-after-free.
    bt_trace("bt_rfcomm_write enter len=%zu", len);
    pthread_mutex_lock(&g_bt_mutex);
    if (!ctx->channel || ctx->state != 1) {
        pthread_mutex_unlock(&g_bt_mutex);
        bt_trace("bt_rfcomm_write exit not-open");
        return -1;
    }
    @autoreleasepool {
        IOReturn r = [ctx->channel writeSync:(void *)data
                                      length:(UInt16)(len & 0xFFFF)];
        pthread_mutex_unlock(&g_bt_mutex);
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
        // Detach the delegate's context pointer UNDER the mutex: a
        // callback already dereferencing ctx holds the mutex until it
        // is done, so this write waits for it; any callback arriving
        // afterwards observes NULL. Merely niling without the mutex
        // races a callback that has passed its NULL check but not yet
        // used the context: the use-after-free this fixes.
        pthread_mutex_lock(&g_bt_mutex);
        ctx->delegate.ctx = NULL;
        ctx->state = -1;
        pthread_mutex_unlock(&g_bt_mutex);

        if (ctx->channel) {
            [ctx->channel setDelegate:nil];
            [ctx->channel closeChannel];
            ctx->channel = nil;
        }
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
