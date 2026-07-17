// Headless Clay demo, used to validate reproit's instrumented backend.
//
// There is NO window, NO GPU, NO real input: Clay is a pure layout library, so
// we just initialize it with an arena, feed a fixed layout-size + a trivial
// text-measure function, then step BeginLayout/EndLayout. reproit_clay.h drives
// the UI by making the chosen button's click-check report true. This is the
// "backend test" shape: it runs in a terminal like a unit test and never
// touches the screen or cursor.
//
// Frame contract (mirrors the header's usage, adapted to Clay's two-phase API):
//   1. BeginLayout + declare the screen (CLAY elements with CLAY_ID + CLAY_TEXT)
//   2. cmds = Clay_EndLayout(dt)        -> render commands now carry the labels
//   3. ReproIt_Clay_Frame(cmds)         -> reset capture, read labels from cmds
//   4. ReproIt_Clay_Clicked(CLAY_ID(x)) -> register each tappable, branch on it
//      (CLAY_ID is a pure hash, valid to call outside the layout; the branch
//       takes effect next frame, exactly like an immediate-mode button press)
//   5. ReproIt_Clay_FrameEnd()          -> emit markers + pick next action
//
// Build (no renderer needed):
//   clang -std=c11 -I . -I ../../runners main.c -o demo
// Run:
//   REPROIT_FUZZ_CONFIG=fuzz.json ./demo
//
// A deliberate "bad path" (the Danger screen's "Boom" button) aborts the
// process, exactly the way an immediate-mode app would crash in production: the
// process dies before printing "All tests passed", which is reproit's
// crash/exception oracle for the Instrumented backend. The normal fuzz walk
// reaches it only if its random pick lands there; the targeted replay
// (fuzz-crash.json) steers straight in so the oracle fires deterministically.

#define CLAY_IMPLEMENTATION
#include "clay.h"

#define REPROIT_CLAY_IMPLEMENTATION
#include "reproit_clay.h"

#include <stdio.h>
#include <stdlib.h>

// Trivial text measurement: width proportional to char count. Clay only needs
// some number to lay out; we never render, so exact metrics do not matter.
static Clay_Dimensions MeasureText(Clay_StringSlice text, Clay_TextElementConfig *config,
                                   void *userData) {
  (void)config;
  (void)userData;
  return (Clay_Dimensions){.width = (float)text.length * 8.0f, .height = 16.0f};
}

static void HandleClayError(Clay_ErrorData err) {
  fprintf(stderr, "CLAY ERROR: %.*s\n", (int)err.errorText.length, err.errorText.chars);
}

enum Screen { HOME, SETTINGS, PLAY, GAMEOVER, DANGER };

// Declare one labeled, clickable button in the layout: a CLAY element with a
// stable CLAY_ID and a CLAY_TEXT child. The hook reads the text (for the state
// signature) from the render commands and uses the id for the click-check.
#define BTN(idStr, labelStr)                                                                       \
  CLAY(CLAY_ID(idStr), {.layout = {.sizing = {CLAY_SIZING_FIXED(160), CLAY_SIZING_FIXED(32)}}}) {  \
    CLAY_TEXT(CLAY_STRING(labelStr), CLAY_TEXT_CONFIG({.fontSize = 16}));                          \
  }

int main(void) {
  uint32_t cap = Clay_MinMemorySize();
  void *mem = malloc(cap);
  Clay_Arena arena = Clay_CreateArenaWithCapacityAndMemory(cap, mem);
  Clay_Initialize(arena, (Clay_Dimensions){1280, 720}, (Clay_ErrorHandler){HandleClayError, 0});
  Clay_SetMeasureTextFunction(MeasureText, 0);

  enum Screen screen = HOME;
  int guard = 0;
  while (!ReproIt_Clay_Done() && guard++ < 5000) {
    // --- phase 1: build the layout for the current screen ---
    Clay_SetLayoutDimensions((Clay_Dimensions){1280, 720});
    Clay_BeginLayout();
    CLAY(CLAY_ID("Root"), {.layout = {.sizing = {CLAY_SIZING_GROW(0), CLAY_SIZING_GROW(0)},
                                      .layoutDirection = CLAY_TOP_TO_BOTTOM,
                                      .childGap = 8,
                                      .padding = CLAY_PADDING_ALL(16)}}) {
      switch (screen) {
      case HOME:
        CLAY_TEXT(CLAY_STRING("Home"), CLAY_TEXT_CONFIG({.fontSize = 24}));
        BTN("Play", "Play");
        BTN("Settings", "Settings");
        break;
      case SETTINGS:
        CLAY_TEXT(CLAY_STRING("Settings"), CLAY_TEXT_CONFIG({.fontSize = 24}));
        BTN("ToggleSound", "Toggle Sound");
        BTN("Danger", "Danger Zone");
        BTN("BackS", "Back");
        break;
      case PLAY:
        CLAY_TEXT(CLAY_STRING("Playing"), CLAY_TEXT_CONFIG({.fontSize = 24}));
        BTN("Pause", "Pause");
        BTN("Quit", "Quit");
        break;
      case GAMEOVER:
        CLAY_TEXT(CLAY_STRING("Game Over"), CLAY_TEXT_CONFIG({.fontSize = 24}));
        BTN("HomeG", "Home");
        break;
      case DANGER:
        CLAY_TEXT(CLAY_STRING("Danger Zone"), CLAY_TEXT_CONFIG({.fontSize = 24}));
        BTN("Boom", "Boom");
        BTN("BackD", "Back");
        break;
      }
    }
    Clay_RenderCommandArray cmds = Clay_EndLayout(1.0f / 60.0f);

    // --- phase 2: hand the commands to the hook, then check clicks ---
    ReproIt_Clay_Frame(cmds); // resets capture, reads labels from commands
    switch (screen) {
    case HOME:
      if (ReproIt_Clay_Clicked(CLAY_ID("Play")))
        screen = PLAY;
      if (ReproIt_Clay_Clicked(CLAY_ID("Settings")))
        screen = SETTINGS;
      break;
    case SETTINGS:
      if (ReproIt_Clay_Clicked(CLAY_ID("ToggleSound"))) { /* toggle, stays */
      }
      if (ReproIt_Clay_Clicked(CLAY_ID("Danger")))
        screen = DANGER;
      if (ReproIt_Clay_Clicked(CLAY_ID("BackS")))
        screen = HOME;
      break;
    case PLAY:
      if (ReproIt_Clay_Clicked(CLAY_ID("Pause"))) { /* stays */
      }
      if (ReproIt_Clay_Clicked(CLAY_ID("Quit")))
        screen = GAMEOVER;
      break;
    case GAMEOVER:
      if (ReproIt_Clay_Clicked(CLAY_ID("HomeG")))
        screen = HOME;
      break;
    case DANGER:
      // The deliberate bad path: this crashes the app. The process
      // aborts before "All tests passed" -> reproit's crash oracle.
      if (ReproIt_Clay_Clicked(CLAY_ID("Boom"))) {
        fprintf(stderr, "REPROIT: deliberate crash on Boom\n");
        fflush(stdout);
        fflush(stderr);
        abort();
      }
      if (ReproIt_Clay_Clicked(CLAY_ID("BackD")))
        screen = SETTINGS;
      break;
    }

    ReproIt_Clay_FrameEnd(); // emit markers + pick next action
  }

  free(mem);
  return 0;
}
