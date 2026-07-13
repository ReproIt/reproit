import QtQuick
import QtQuick.Controls

ApplicationWindow {
    width: 480
    height: 240
    visible: true
    title: "ReproQml"

    Column {
        anchors.centerIn: parent
        spacing: 12

        Rectangle {
            id: toggle
            width: 120
            height: 44
            color: "#dddddd"
            border.color: "#555555"
            focus: true
            Accessible.name: "toggle"
            Accessible.role: Accessible.Button
            Accessible.onPressAction: extra.visible = !extra.visible

            Text {
                anchors.centerIn: parent
                text: "Toggle"
            }

            MouseArea {
                anchors.fill: parent
                onClicked: extra.visible = !extra.visible
            }
        }

        Text {
            text: extra.visible ? "open" : "closed"
            Accessible.name: "status"
            Accessible.role: Accessible.StaticText
        }

        Text {
            id: extra
            text: "Detail revealed"
            visible: false
            Accessible.name: "extra"
            Accessible.role: Accessible.StaticText
        }
    }
}
