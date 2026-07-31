/* A fixed timestep loop built on SDL2, driven by an input stream on stdin.
 *
 * The loop, the timing source, and the event pump are SDL's. Input arrives on
 * stdin because that is how a headless engine is driven by a demo or replay
 * file: there is no evdev or X11 in a container, and an engine that cannot be
 * driven headless cannot be tested at all.
 *
 * The planted defect is a STALE COMBO: the code assumes a combo's presses
 * arrive close together, so it never clears the buffer when they do not, and
 * a press arriving more than STALE_AFTER frames after the previous one reads
 * a stale slot and trips the assertion.
 *
 * That direction is deliberate. The same three bytes arriving BACK TO BACK are
 * safe, and arriving SPREAD OUT crash. So a replay that delivered the recorded
 * input immediately, instead of on the tick it arrived on, would NOT reproduce
 * the crash. The bug is in the schedule, not the bytes.
 */
#include <SDL2/SDL.h>
#include <assert.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#define STALE_AFTER 6

int main(void) {
    if (SDL_Init(SDL_INIT_TIMER | SDL_INIT_VIDEO) != 0) {
        fprintf(stderr, "SDL_Init: %s\n", SDL_GetError());
        return 2;
    }
    int flags = fcntl(0, F_GETFL);
    fcntl(0, F_SETFL, flags | O_NONBLOCK);

    const char *frames_env = getenv("ENGINE_FRAMES");
    unsigned budget = frames_env ? (unsigned)atoi(frames_env) : 120;
    int safe = getenv("REPROIT_FIXED") != NULL;

    unsigned last_press = 0;
    int have_press = 0;
    int combo = 0;

    for (unsigned frame = 0; frame < budget; frame++) {
        Uint32 began = SDL_GetTicks();
        SDL_Event ev;
        while (SDL_PollEvent(&ev)) {
        }
        char c;
        if (read(0, &c, 1) == 1 && c == 'u') {
            unsigned gap = have_press ? frame - last_press : 0;
            if (have_press && gap > STALE_AFTER) {
                if (safe) {
                    combo = 0; /* the fix: a stale combo is discarded */
                } else {
                    /* the defect: the slot is reused while stale */
                    combo++;
                    printf("frame %u stale gap %u combo %d\n", frame, gap, combo);
                    fflush(stdout);
                    assert(gap <= STALE_AFTER && "stale combo slot reused");
                }
            }
            combo++;
            last_press = frame;
            have_press = 1;
            printf("frame %u press combo %d\n", frame, combo);
            fflush(stdout);
        }
        while (SDL_GetTicks() - began < 5) {
            SDL_Delay(1);
        }
    }
    printf("survived\n");
    SDL_Quit();
    return 0;
}
