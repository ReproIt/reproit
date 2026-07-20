use super::*;

pub(super) fn grab_focus(acc: &Acc) {
    unsafe {
        let comp = atspi_accessible_get_component_iface(acc.ptr());
        if !comp.is_null() {
            let _ = atspi_component_grab_focus(comp, ptr::null_mut());
            g_object_unref(comp);
        }
    }
}

pub(super) fn do_press(acc: &Acc) -> bool {
    unsafe {
        let ai = atspi_accessible_get_action_iface(acc.ptr());
        if ai.is_null() {
            return false;
        }
        let n = atspi_action_get_n_actions(ai, ptr::null_mut());
        let ok = n > 0 && atspi_action_do_action(ai, 0, ptr::null_mut()) != 0;
        g_object_unref(ai);
        ok
    }
}

pub(super) fn set_text(acc: &Acc, value: &str) -> bool {
    unsafe {
        let et = atspi_accessible_get_editable_text_iface(acc.ptr());
        if et.is_null() {
            return false;
        }
        let c = CString::new(value).unwrap_or_default();
        let ok = atspi_editable_text_set_text_contents(et, c.as_ptr(), ptr::null_mut()) != 0;
        g_object_unref(et);
        ok
    }
}

pub(super) fn send_escape() {
    unsafe {
        let _ = atspi_generate_keyboard_event(
            XKEYCODE_ESCAPE,
            ptr::null(),
            ATSPI_KEY_PRESSRELEASE,
            ptr::null_mut(),
        );
    }
}

pub(super) fn find_typable(node: &Acc, finder: &str, depth: usize) -> Option<Acc> {
    if depth > 60 {
        return None;
    }
    let want = finder.strip_prefix("key:").unwrap_or(finder);
    let rn = role_name(node);
    if TYPABLE_ROLE_NAMES.contains(&rn.as_str()) {
        let ident = acc_id(node).unwrap_or_default();
        let label = acc_name(node);
        if (!ident.is_empty() && (ident == want || ident == finder))
            || (!label.is_empty() && label == want)
        {
            return Some(node.dup());
        }
    }
    for child in acc_children(node) {
        if let Some(hit) = find_typable(&child, finder, depth + 1) {
            return Some(hit);
        }
    }
    None
}

pub(super) fn reset_to_root() {
    for _ in 0..4 {
        send_escape();
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_millis(400));
}
