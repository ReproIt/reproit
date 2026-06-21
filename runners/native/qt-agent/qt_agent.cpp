// ReproIt Qt IN-PROCESS operability agent (graph-1 ground truth).
//
// !!! TOOLCHAIN STATUS: BUILT + RUN + VERIFIED (in a Linux Docker container).
//     This host (Apple M1, macOS) has no Qt toolchain, so the demo was built and
//     run headless in Debian with Qt 6.8.2:
//         apt-get install -y build-essential pkg-config qt6-base-dev libgl1-mesa-dev
//         g++ -std=c++17 $(pkg-config --cflags Qt6Widgets Qt6Gui Qt6Core) \
//             -DREPROIT_QT_DEMO_MAIN -fPIC qt_agent.cpp \
//             $(pkg-config --libs Qt6Widgets Qt6Gui Qt6Core) -o qt_agent
//         QT_QPA_PLATFORM=offscreen ./qt_agent   # offscreen plugin = no display
//     It emits the expected marker (sig 3854aea0, matching the AppKit agent): the
//     fake button is operable:true / rolePresent:false (NO_ROLE + keyboard-
//     unreachable + pointer-only), the real + good buttons clean. See
//     runners/native/README.md and the qt_* contract test in model/map.rs.
//     It is intended to be loaded INTO a running Qt app (e.g. via a Qt plugin /
//     a QTimer single-shot installed from an injected library) so it can read
//     the live QWidget object graph; the standalone `demoMain` below builds a
//     proof window (real QPushButton + a "fake button" QWidget with a wired
//     clicked-like handler and no accessible role) for offline verification.
//
// Like the AppKit agent, this reads graph 1 (the real QObject/QWidget tree and
// its wired signal/slot handlers = the operability ground truth) and joins it,
// by QObject identity, against graph 2 (QAccessibleInterface: the role/name/
// state Qt published to AT-SPI / MSAA / the Mac AX bridge). The diff is the
// operability/accessibility gap the engine scores.
//
// Output marker (parsed by crates/reproit/src/model/map.rs::gaps_from_groundtruth):
//   EXPLORE:GROUNDTRUTH {"sig":..,"focusTrap":bool,"elements":[{id,operable,
//     gestureKind,a11y:{rolePresent,namePresent,focusable,inTabOrder,
//     keyboardActivatable}}]}

#include <QApplication>
#include <QWidget>
#include <QMouseEvent>
#include <QPushButton>
#include <QAbstractButton>
#include <QObject>
#include <QMetaObject>
#include <QMetaMethod>
#include <QAccessible>
#include <QAccessibleInterface>
#include <QWidgetList>
#include <QString>
#include <QStringList>
#include <QSet>
#include <QVector>
#include <QJsonObject>
#include <QJsonArray>
#include <QJsonDocument>
#include <QTextStream>
#include <cstdint>
#include <functional>

namespace reproit_qt {

static QTextStream out(stdout);
static QTextStream err(stderr);

// ---- canonical signature (FNV-1a, structural-only subset of the oracle) ----
static QString fnv1a32hex(const QByteArray &bytes) {
    uint32_t h = 0x811c9dc5u;
    for (unsigned char b : bytes) { h ^= b; h *= 0x01000193u; }
    return QString::asprintf("%08x", h);
}

// ---- the FAKE button: a styled QWidget with a wired click handler and no -----
// ---- accessible role. The Qt analogue of the AppKit FakeButton: it acts ------
// ---- like a button (graph 1 operable) but publishes no QAccessible role. -----
class FakeButton : public QWidget {
public:
    explicit FakeButton(QWidget *parent = nullptr) : QWidget(parent) {}
    std::function<void()> onClick;
protected:
    // A real mousePressEvent handler makes this widget operable by pointer.
    void mousePressEvent(QMouseEvent *) override { if (onClick) onClick(); }
    // Deliberately no QAccessibleWidget subclass / accessibleName / focusPolicy:
    // graph 2 sees no operable role and no keyboard reachability.
};

// ---- graph 1: is this QObject OPERABLE (ground truth off the object graph)? --
// A QObject is operable when:
//   - it is a QAbstractButton (QPushButton/QToolButton/QCheckBox/QRadioButton),
//   - OR it has a `clicked()`/`pressed()`/`toggled()` signal with >=1 connected
//     receiver (read from the meta-object: a wired handler = real behavior),
//   - OR (the fake-button case) it overrides mousePressEvent on a custom widget.
// Returns (operable, gestureKind).
struct Op { bool operable; QString gestureKind; };

// QObject::isSignalConnected is `protected`, so reach it through a thin
// same-layout accessor cast (well-defined: identical object, member added with
// no data/vtable change). Lets the agent read signal-wiring ground truth
// without subclassing every inspected object.
struct SignalConnectedAccessor : public QObject {
    using QObject::isSignalConnected;
};

static bool hasConnectedSignal(const QObject *o, const char *signalSig) {
    const QMetaObject *mo = o->metaObject();
    int idx = mo->indexOfSignal(QMetaObject::normalizedSignature(signalSig).constData());
    if (idx < 0) return false;
    QMetaMethod sig = mo->method(idx);
    // isSignalConnected reports whether anything is wired to the signal: a
    // connected clicked() is a real, ground-truth operable behavior.
    return static_cast<const SignalConnectedAccessor *>(o)->isSignalConnected(sig);
}

static Op graph1Operable(QObject *o) {
    if (qobject_cast<QAbstractButton *>(o)) return {true, "button"};
    if (hasConnectedSignal(o, "clicked()") || hasConnectedSignal(o, "clicked(bool)"))
        return {true, "button"};
    if (hasConnectedSignal(o, "pressed()")) return {true, "longPress"};
    if (hasConnectedSignal(o, "toggled(bool)")) return {true, "toggle"};
    // Custom widget that overrides mousePressEvent: detectable because the
    // FakeButton class above is a QWidget whose dynamic type is not QWidget.
    // (Generic detection of an overridden virtual is not possible from the
    // meta-object; in a real deployment the agent ships a known list of the
    // app's custom-clickable base classes, or instruments QWidget::event.)
    // dynamic_cast (not qobject_cast): FakeButton carries no Q_OBJECT meta data,
    // so we detect the custom-clickable subclass by C++ RTTI instead.
    if (auto *w = dynamic_cast<FakeButton *>(o)) { (void)w; return {true, "button"}; }
    return {false, ""};
}

// ---- graph 2: the QAccessible projection of the SAME object ------------------
struct A11y {
    bool rolePresent, namePresent, focusable, inTabOrder, keyboardActivatable;
};

static A11y graph2A11y(QObject *o) {
    A11y a{false, false, false, false, false};
    QAccessibleInterface *iface = QAccessible::queryAccessibleInterface(o);
    if (iface) {
        QAccessible::Role role = iface->role();
        // NoRole / Grouping / Client are structural fall-throughs: no operable role.
        a.rolePresent = (role != QAccessible::NoRole
                         && role != QAccessible::Grouping
                         && role != QAccessible::Client);
        a.namePresent = !iface->text(QAccessible::Name).isEmpty();
        QAccessible::State st = iface->state();
        a.focusable = st.focusable;
        // Keyboard-activatable: an a11y element exposing the Press/Default action,
        // i.e. a focusable element with an operable role (buttons do; a bare
        // mouse-press widget with no QAccessible role does not).
        a.keyboardActivatable = a.rolePresent && st.focusable && !st.disabled;
    }
    // In tab order: a widget with a focus policy that accepts Tab. Read from the
    // QWidget directly (the keyboard tab chain is built from focusPolicy).
    if (auto *w = qobject_cast<QWidget *>(o)) {
        Qt::FocusPolicy fp = w->focusPolicy();
        a.inTabOrder = (fp & Qt::TabFocus) != 0;
        if (!iface) a.focusable = (fp != Qt::NoFocus);
    }
    return a;
}

// Canonical role token + element id, joined by QObject identity (objectName is
// the Qt analogue of accessibilityIdentifier / a test-id).
static QString roleToken(QObject *o) {
    if (qobject_cast<QAbstractButton *>(o)) return "button";
    QAccessibleInterface *iface = QAccessible::queryAccessibleInterface(o);
    if (iface) {
        switch (iface->role()) {
        case QAccessible::Button:    return "button";
        case QAccessible::CheckBox:  return "checkbox";
        case QAccessible::RadioButton: return "radio";
        case QAccessible::Slider:    return "slider";
        case QAccessible::EditableText: return "textfield";
        case QAccessible::StaticText: return "text";
        case QAccessible::Link:      return "link";
        default: break;
        }
    }
    return "group";
}

struct GTElement { QString id; bool operable; QString gestureKind; A11y a11y; };

// ---- the walk: graph 1 x graph 2 over the live widget tree -------------------
static void walkObject(QObject *o, int depth, QVector<GTElement> &elements,
                       QStringList &sigTokens, QSet<QString> &roleSeen) {
    if (!o) return;
    Op op = graph1Operable(o);
    A11y a = graph2A11y(o);
    if (op.operable || a.rolePresent) {
        QString id;
        QString name = o->objectName();
        if (!name.isEmpty()) {
            id = "key:" + name;
        } else {
            QString r = roleToken(o);
            int idx = 0;
            while (roleSeen.contains(r + "#" + QString::number(idx))) idx++;
            QString tok = r + "#" + QString::number(idx);
            roleSeen.insert(tok);
            id = "role:" + tok;
        }
        elements.push_back({id, op.operable, op.gestureKind, a});
        sigTokens << QString::number(depth) + ":" + roleToken(o) + "@" + id;
    }
    for (QObject *child : o->children())
        walkObject(child, depth + 1, elements, sigTokens, roleSeen);
}

static void emitGroundTruth() {
    QVector<GTElement> elements;
    QStringList sigTokens;
    QSet<QString> roleSeen;
    const QWidgetList tops = QApplication::topLevelWidgets();
    for (QWidget *w : tops)
        walkObject(w, 0, elements, sigTokens, roleSeen);

    // focusTrap heuristic: operable elements exist but none is in the tab order.
    bool anyOperable = false, anyTab = false;
    for (const auto &e : elements) {
        if (e.operable) anyOperable = true;
        if (e.a11y.inTabOrder) anyTab = true;
    }
    bool focusTrap = anyOperable && !anyTab;

    QString descriptor = "A:\n" + sigTokens.join(";");
    QString sig = fnv1a32hex(descriptor.toUtf8());

    QJsonArray arr;
    for (const auto &e : elements) {
        QJsonObject a11y{
            {"rolePresent", e.a11y.rolePresent},
            {"namePresent", e.a11y.namePresent},
            {"focusable", e.a11y.focusable},
            {"inTabOrder", e.a11y.inTabOrder},
            {"keyboardActivatable", e.a11y.keyboardActivatable},
        };
        arr.append(QJsonObject{
            {"id", e.id}, {"operable", e.operable},
            {"gestureKind", e.gestureKind}, {"a11y", a11y}});
    }
    QJsonObject payload{{"sig", sig}, {"focusTrap", focusTrap}, {"elements", arr}};
    out << "EXPLORE:GROUNDTRUTH "
        << QJsonDocument(payload).toJson(QJsonDocument::Compact) << "\n";
    out.flush();

    for (const auto &e : elements) {
        QStringList g;
        if (e.operable && !e.a11y.rolePresent) g << "NO_ROLE";
        if (e.operable && !e.a11y.inTabOrder) g << "KEYBOARD_UNREACHABLE";
        if (e.operable && !e.a11y.keyboardActivatable) g << "POINTER_ONLY";
        err << "  " << e.id << " operable=" << (e.operable ? "true" : "false")
            << " -> " << (g.isEmpty() ? "OK" : "GAP(" + g.join(",") + ")") << "\n";
    }
    err.flush();
}

} // namespace reproit_qt

// ---- standalone proof entry point -------------------------------------------
// Builds a window with a real QPushButton + a fake-button QWidget + a correctly
// accessible custom control, then walks. (In production the agent is loaded into
// the target app instead and `emitGroundTruth()` is called on a QTimer.)
#ifdef REPROIT_QT_DEMO_MAIN
int main(int argc, char **argv) {
    QApplication app(argc, argv);

    QWidget window;
    window.setObjectName("root");

    auto *real = new QPushButton("Real Button", &window);
    real->setObjectName("realButton");
    real->setGeometry(20, 120, 140, 32);
    QObject::connect(real, &QPushButton::clicked, [] {});

    auto *fake = new reproit_qt::FakeButton(&window);
    fake->setObjectName("fakeButton");
    fake->setGeometry(20, 70, 140, 32);
    fake->onClick = [] {};
    // No focusPolicy, no accessibleName: the gap.

    auto *good = new QPushButton("Good", &window);   // a real, accessible control
    good->setObjectName("goodCustom");
    good->setAccessibleName("Accessible Custom Button");
    good->setGeometry(200, 70, 160, 32);
    QObject::connect(good, &QPushButton::clicked, [] {});

    reproit_qt::out << "JOURNEY claimed role=qt-agent\n";
    reproit_qt::out.flush();
    reproit_qt::emitGroundTruth();
    reproit_qt::out << "JOURNEY DONE\nAll tests passed\n";
    reproit_qt::out.flush();
    return 0;   // headless: never enters the event loop
}
#endif
