// Headless Dear ImGui demo, used to validate reproit's instrumented backend.
//
// There is NO window, NO GPU, NO real input: ImGui runs against a null backend
// (we just feed io.DisplaySize + io.DeltaTime and build the font atlas, then
// step NewFrame/Render). reproit_imgui.h drives the UI by making the chosen
// button report a press. This is the "backend test" shape: it runs in a
// terminal like a unit test and never touches the screen or cursor.
//
// Build (no GLFW/OpenGL needed):
//   clang++ -std=c++17 -I imgui -I ../../runners main.cpp imgui/*.cpp -o demo
// Run:
//   REPROIT_FUZZ_CONFIG=fuzz.json ./demo

#include "imgui.h"
#define REPROIT_IMGUI_IMPLEMENTATION
#include "reproit_imgui.h"

enum Screen { HOME, SETTINGS, PLAY, GAMEOVER };

int main() {
  IMGUI_CHECKVERSION();
  ImGui::CreateContext();
  ImGuiIO &io = ImGui::GetIO();
  io.DisplaySize = ImVec2(1280, 720);
  io.DeltaTime = 1.0f / 60.0f;
  // Build the font atlas so NewFrame() has what it needs (still no GPU).
  unsigned char *pixels = nullptr;
  int w = 0, h = 0;
  io.Fonts->GetTexDataAsRGBA32(&pixels, &w, &h);

  Screen screen = HOME;
  int guard = 0;
  while (!reproit::Done() && guard++ < 5000) {
    ImGui::NewFrame();
    reproit::Frame();

    ImGui::Begin("App");
    switch (screen) {
    case HOME:
      ImGui::Text("Home");
      if (reproit::Button("Play"))
        screen = PLAY;
      if (reproit::Button("Settings"))
        screen = SETTINGS;
      break;
    case SETTINGS:
      ImGui::Text("Settings");
      if (reproit::Button("Toggle Sound")) { /* a toggle, stays here */
      }
      if (reproit::Button("Back"))
        screen = HOME;
      break;
    case PLAY:
      ImGui::Text("Playing");
      if (reproit::Button("Pause")) { /* stays */
      }
      if (reproit::Button("Quit"))
        screen = GAMEOVER;
      break;
    case GAMEOVER:
      ImGui::Text("Game Over");
      if (reproit::Button("Home"))
        screen = HOME;
      break;
    }
    ImGui::End();

    reproit::FrameEnd();
    ImGui::Render();
  }
  ImGui::DestroyContext();
  return 0;
}
