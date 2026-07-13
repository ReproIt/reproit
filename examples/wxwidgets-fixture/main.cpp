#include <wx/wx.h>

class FixtureFrame final : public wxFrame {
public:
    FixtureFrame() : wxFrame(nullptr, wxID_ANY, "Reproit wxWidgets Fixture", wxDefaultPosition, wxSize(420, 300)) {
        auto* panel = new wxPanel(this);
        auto* layout = new wxBoxSizer(wxVERTICAL);
        status = new wxStaticText(panel, wxID_ANY, "Status");
        status->SetName("status");
        auto* toggle = new wxButton(panel, wxID_ANY, "Toggle");
        toggle->SetName("toggle");
        extra = new wxStaticText(panel, wxID_ANY, "Extra revealed");
        extra->SetName("extra");
        extra->Hide();
        toggle->Bind(wxEVT_BUTTON, [this](wxCommandEvent&) {
            extra->Show(!extra->IsShown());
            Layout();
        });
        layout->Add(status, 0, wxALL, 12);
        layout->Add(toggle, 0, wxALL, 12);
        layout->Add(extra, 0, wxALL, 12);
        panel->SetSizer(layout);
    }

private:
    wxStaticText* status;
    wxStaticText* extra;
};

class FixtureApp final : public wxApp {
public:
    bool OnInit() override {
        (new FixtureFrame())->Show();
        return true;
    }
};

wxIMPLEMENT_APP(FixtureApp);
