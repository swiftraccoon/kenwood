#ifdef __APPLE__

#import <Foundation/Foundation.h>
#import <IOBluetooth/IOBluetooth.h>
#include <fcntl.h>
#include <pthread.h>
#include <unistd.h>

@class RfcommDelegate;

typedef struct {
    IOBluetoothDevice *device;
    IOBluetoothRFCOMMChannel *channel;
    RfcommDelegate *delegate;
    int pipe_read;
    int pipe_write;
    volatile int state;
} RfcommContext;

@interface RfcommDelegate : NSObject <IOBluetoothRFCOMMChannelDelegate>
@property (nonatomic, assign) RfcommContext *ctx;
@end

@implementation RfcommDelegate
- (void)rfcommChannelOpenComplete:(IOBluetoothRFCOMMChannel *)ch status:(IOReturn)e {
    if (_ctx && e == kIOReturnSuccess) _ctx->state = 1;
}
- (void)rfcommChannelData:(IOBluetoothRFCOMMChannel *)ch data:(void *)data length:(size_t)len {
    if (_ctx && _ctx->pipe_write >= 0) write(_ctx->pipe_write, data, len);
}
- (void)rfcommChannelClosed:(IOBluetoothRFCOMMChannel *)ch {
    if (_ctx) _ctx->state = 0;
}
@end

static pthread_t g_pump_thread;
static volatile int g_pump_running = 0;

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
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, false);
    } else {
        // From a non-main thread, wake the main run loop so it processes
        // pending IOBluetooth callbacks. The pump_main_runloop background
        // thread also does this, but an explicit wake ensures timely delivery.
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
            close(ctx->pipe_read); close(ctx->pipe_write);
            free(ctx); return NULL;
        }

        for (int i = 0; i < 200 && ctx->state == 0; i++)
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, false);

        if (ctx->state != 1) {
            if (channel) [channel closeChannel];
            close(ctx->pipe_read); close(ctx->pipe_write);
            free(ctx); return NULL;
        }

        ctx->channel = channel;
        return ctx;
    }
}


void *bt_rfcomm_open(const char *device_name, uint8_t rfcomm_channel) {
    // fprintf(stderr, "BT: bt_rfcomm_open called, main=%d\n", [NSThread isMainThread]);
    g_pump_running = 1;
    pthread_create(&g_pump_thread, NULL, pump_main_runloop, NULL);
    void *r = do_rfcomm_open(device_name, rfcomm_channel);
    // fprintf(stderr, "BT: bt_rfcomm_open result=%p\n", r);
    return r;
}

int bt_rfcomm_write(void *handle, const uint8_t *data, size_t len) {
    RfcommContext *ctx = (RfcommContext *)handle;
    if (!ctx || !ctx->channel || ctx->state != 1) return -1;
    @autoreleasepool {
        return ([ctx->channel writeSync:(void *)data
                                 length:(UInt16)(len & 0xFFFF)] == kIOReturnSuccess) ? 0 : -1;
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
        if (ctx->channel) { [ctx->channel closeChannel]; ctx->channel = nil; }
        ctx->state = -1;
        g_pump_running = 0;
        pthread_join(g_pump_thread, NULL);
        if (ctx->pipe_write >= 0) { close(ctx->pipe_write); ctx->pipe_write = -1; }
        ctx->delegate.ctx = NULL;
        free(ctx);
    }
}

#endif
