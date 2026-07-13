// Minimal REAL Qt6 Widgets app used to prove reproit drives a native Qt UI
// through the AT-SPI a11y backend (runners/linux-atspi.py).
//
// The window exposes a single "Toggle" QPushButton. Clicking it flips a bool
// and SHOWS/HIDES a second widget (the "extra" panel). Showing/hiding a widget
// adds/removes a node from the AT-SPI accessibility tree, so the tap moves the
// app to a structurally different state: exactly the change reproit's canonical
// structural signature is built to detect (a label TEXT change alone would not
// move the signature, so the toggle is deliberately structural).
//
// Every interactive/reported widget carries an accessibleName so it surfaces in
// the AT-SPI tree with a stable id the runner can read as a `key:` selector.
#include <QApplication>
#include <QWidget>
#include <QVBoxLayout>
#include <QPushButton>
#include <QLabel>

int main(int argc, char **argv) {
    QApplication app(argc, argv);

    QWidget win;
    win.setWindowTitle("ReproQt");
    win.setAccessibleName("reproqt-window");

    QVBoxLayout *layout = new QVBoxLayout(&win);

    QLabel *status = new QLabel("Off");
    status->setAccessibleName("status");
    layout->addWidget(status);

    // The structural toggle target: hidden until the button is pressed.
    QLabel *extra = new QLabel("Extra panel");
    extra->setAccessibleName("extra");
    extra->setVisible(false);
    layout->addWidget(extra);

    QPushButton *btn = new QPushButton("Toggle");
    btn->setAccessibleName("toggle");
    layout->addWidget(btn);

    bool *on = new bool(false);
    QObject::connect(btn, &QPushButton::clicked, [=]() {
        *on = !*on;
        status->setText(*on ? "On" : "Off");
        extra->setVisible(*on);
    });

    win.resize(320, 200);
    win.show();
    return app.exec();
}
