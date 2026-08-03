//! UI Automation control-type classification and window-chrome exclusions.

pub(super) const TAPPABLE_CONTROL_TYPES: &[i32] = &[
    50000, // Button
    50011, // MenuItem
    50019, // TabItem
    50007, // ListItem
    50005, // Hyperlink
    50002, // CheckBox
    50013, // RadioButton
];

const TITLEBAR_CONTROL_TYPE: i32 = 50037;
const BUTTON_CONTROL_TYPE: i32 = 50000;
const TITLEBAR_AUTOMATION_ID: &str = "TitleBar";
const CAPTION_BUTTON_AUTOMATION_IDS: &[&str] = &["Close", "Minimize", "Maximize", "Restore"];

pub(super) fn uia_role(control_type: i32) -> &'static str {
    match control_type {
        50000 => "button",
        50001 => "group",
        50002 => "checkbox",
        50003 | 50004 => "textfield",
        50005 => "link",
        50006 => "image",
        50007 => "listitem",
        50008 => "list",
        50009 | 50010 => "menu",
        50011 => "menuitem",
        50012 => "progress",
        50013 => "radio",
        50014 => "node",
        50015 => "slider",
        50016 => "spinner",
        50017 => "text",
        50018 | 50019 => "tab",
        50020 => "text",
        50021 => "group",
        50022 => "tooltip",
        50023 => "list",
        50024 => "listitem",
        50025 | 50026 => "group",
        50027 => "node",
        50028 => "list",
        50029 => "listitem",
        50030 => "textfield",
        50031 => "button",
        50032 => "screen",
        50033 => "group",
        50034 | 50035 => "header",
        50036 => "list",
        50037 => "header",
        50038 => "node",
        _ => "node",
    }
}

pub(super) fn is_titlebar_root(control_type: i32, automation_id: Option<&str>) -> bool {
    control_type == TITLEBAR_CONTROL_TYPE || automation_id == Some(TITLEBAR_AUTOMATION_ID)
}

pub(super) fn is_caption_button(control_type: i32, automation_id: Option<&str>) -> bool {
    control_type == BUTTON_CONTROL_TYPE
        && automation_id.is_some_and(|id| CAPTION_BUTTON_AUTOMATION_IDS.contains(&id))
}
