/* A test pointer for the e2e (wlr virtual pointer), run in the nested Hyprland only.
 * Usage: e2e_pointer CMD...   CMD: abs:X,Y,W,H | rel:DX,DY | down | up | click | sleep:MS
 * abs: absolute position X,Y in a W x H layout. Button: the left one. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"

static struct zwlr_virtual_pointer_manager_v1 *manager;
static struct wl_seat *seat;

static void global(void *data, struct wl_registry *r, uint32_t name, const char *iface, uint32_t version) {
    if (!strcmp(iface, zwlr_virtual_pointer_manager_v1_interface.name))
        manager = wl_registry_bind(r, name, &zwlr_virtual_pointer_manager_v1_interface, 1);
    else if (!strcmp(iface, wl_seat_interface.name) && !seat)
        seat = wl_registry_bind(r, name, &wl_seat_interface, 1);
}
static void global_remove(void *data, struct wl_registry *r, uint32_t name) {}
static const struct wl_registry_listener listener = {global, global_remove};

static uint32_t now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint32_t)(t.tv_sec * 1000 + t.tv_nsec / 1000000);
}

int main(int argc, char **argv) {
    struct wl_display *d = wl_display_connect(NULL);
    if (!d) { fprintf(stderr, "no wayland display\n"); return 1; }
    struct wl_registry *reg = wl_display_get_registry(d);
    wl_registry_add_listener(reg, &listener, NULL);
    wl_display_roundtrip(d);
    if (!manager) { fprintf(stderr, "no zwlr_virtual_pointer_manager_v1\n"); return 1; }
    struct zwlr_virtual_pointer_v1 *p = zwlr_virtual_pointer_manager_v1_create_virtual_pointer(manager, seat);
    for (int i = 1; i < argc; i++) {
        const char *c = argv[i];
        unsigned x, y, w, h; int dx, dy, ms;
        if (sscanf(c, "abs:%u,%u,%u,%u", &x, &y, &w, &h) == 4) {
            zwlr_virtual_pointer_v1_motion_absolute(p, now_ms(), x, y, w, h);
            zwlr_virtual_pointer_v1_frame(p);
        } else if (sscanf(c, "rel:%d,%d", &dx, &dy) == 2) {
            zwlr_virtual_pointer_v1_motion(p, now_ms(), wl_fixed_from_int(dx), wl_fixed_from_int(dy));
            zwlr_virtual_pointer_v1_frame(p);
        } else if (!strcmp(c, "down") || !strcmp(c, "up") || !strcmp(c, "click")) {
            if (strcmp(c, "up")) { zwlr_virtual_pointer_v1_button(p, now_ms(), 0x110, 1); zwlr_virtual_pointer_v1_frame(p); }
            if (!strcmp(c, "click")) { wl_display_roundtrip(d); usleep(60000); }
            if (strcmp(c, "down")) { zwlr_virtual_pointer_v1_button(p, now_ms(), 0x110, 0); zwlr_virtual_pointer_v1_frame(p); }
        } else if (sscanf(c, "sleep:%d", &ms) == 1) {
            wl_display_roundtrip(d);
            usleep(ms * 1000);
            continue;
        } else {
            fprintf(stderr, "unknown command %s\n", c);
            return 2;
        }
        wl_display_roundtrip(d);
    }
    zwlr_virtual_pointer_v1_destroy(p);
    wl_display_roundtrip(d);
    wl_display_disconnect(d);
    return 0;
}
