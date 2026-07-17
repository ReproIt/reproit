#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QUrl>

int main(int argc, char **argv) {
  QGuiApplication app(argc, argv);
  QQmlApplicationEngine engine;
  engine.load(QUrl::fromLocalFile("/work/main.qml"));
  if (engine.rootObjects().isEmpty())
    return 2;
  return app.exec();
}
