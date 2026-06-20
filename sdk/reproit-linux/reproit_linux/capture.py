"""In-app widget-tree capture for native Linux GUI apps (GTK / Qt).

Two capture paths share ONE node-descriptor model (signature.Node) and ONE role
map (the AT-SPI Role name -> canonical role vocabulary), so a GTK app and a Qt
app fold into the same descriptor and hash identically:

  1. GTK / ATK in-process walk (build_node_atk). A GTK app already holds its
     widget tree; every GTK widget exposes an ATK accessible via
     `widget.get_accessible()` (GTK3) or the GTK4 accessible interface. We walk
     that accessible tree (roles, names, text, editable value) WITHOUT going out
     to the AT-SPI bus, because the SDK runs inside the app's own process. ATK
     and AT-SPI use the SAME role enumeration (atk Role names match the AT-SPI
     role names, e.g. PUSH_BUTTON), so the same role map serves both.

  2. AT-SPI accessible walk (build_node_atspi). Qt apps (and any toolkit that
     publishes to the accessibility bus) expose AT-SPI accessibles. This walk is
     the in-app twin of runners/linux-atspi.py's build_node: same role map, same
     id / type / value extraction, folded into the same Node. It is designed so
     a Qt app can be covered by the identical accessibility-based walk.

Localized chrome name/text is excluded by construction (rule 1): `value` is read
only for value-bearing roles (docs/signature.md "Value-state"), so the V:
section carries the bounded value-class while the structural body stays
text-free.

The role map and the pure mapping helpers (atk_role, node_from_attrs) have NO
GTK/AT-SPI import, so they are unit-testable on any host. The live walks defer
the `gi` import.
"""

from .signature import Node

MAX_DEPTH = 60


# =============================================================================
# AT-SPI / ATK Role name -> canonical role vocabulary.
# Keyed by the Role *name* (string, e.g. "PUSH_BUTTON"), shared by both the GTK
# ATK walk and the AT-SPI walk because ATK and AT-SPI use the same role names.
# This map is identical to runners/linux-atspi.py's ATSPI_ROLE_TO_ROLE so the
# in-app SDK and the external runner fold the same widget to the same role.
# Roles outside the vocabulary fall through to `node` via normalize_role.
# =============================================================================

ATSPI_ROLE_TO_ROLE = {
    "FRAME": "screen",
    "WINDOW": "screen",
    "APPLICATION": "screen",
    "DIALOG": "dialog",
    "ALERT": "dialog",
    "FILE_CHOOSER": "dialog",
    "COLOR_CHOOSER": "dialog",
    "HEADING": "header",
    "PAGE_TAB_LIST": "tab",
    "PAGE_TAB": "tab",
    "LABEL": "text",
    "TEXT": "textfield",
    "ENTRY": "textfield",
    "PASSWORD_TEXT": "textfield",
    "PARAGRAPH": "text",
    "STATIC": "text",
    "CAPTION": "text",
    "PUSH_BUTTON": "button",
    "TOGGLE_BUTTON": "button",
    "SPIN_BUTTON": "textfield",
    "LINK": "link",
    "IMAGE": "image",
    "ICON": "image",
    "LIST": "list",
    "LIST_BOX": "list",
    "TABLE": "list",
    "TREE": "list",
    "TREE_TABLE": "list",
    "LIST_ITEM": "listitem",
    "TABLE_ROW": "listitem",
    "TABLE_CELL": "listitem",
    "TREE_ITEM": "listitem",
    "CHECK_BOX": "checkbox",
    "CHECK_MENU_ITEM": "checkbox",
    "RADIO_BUTTON": "radio",
    "RADIO_MENU_ITEM": "radio",
    "SWITCH": "switch",
    "TOGGLE_SWITCH": "switch",
    "SLIDER": "slider",
    "SCROLL_BAR": "node",
    "PROGRESS_BAR": "progress",   # transient -> dropped (unless value-bearing)
    "SPINNER": "spinner",         # transient -> dropped
    "BUSY_INDICATOR": "spinner",  # transient -> dropped
    "TOOL_TIP": "tooltip",        # transient -> dropped
    "NOTIFICATION": "toast",      # transient -> dropped
    "INFO_BAR": "toast",          # transient -> dropped
    "MENU": "menu",
    "MENU_BAR": "menu",
    "POPUP_MENU": "menu",
    "MENU_ITEM": "menuitem",
    "PANEL": "group",
    "FILLER": "group",
    "GROUPING": "group",
    "TOOL_BAR": "group",
    "VIEWPORT": "group",
    "SECTION": "group",
    "FORM": "group",
    "SCROLL_PANE": "group",
    "SPLIT_PANE": "group",
    "LAYERED_PANE": "group",
    "SEPARATOR": "node",
    "STATUS_BAR": "text",
}

# Role names whose name signals an input -> `type` refinement.
INPUT_TYPE_BY_ROLE = {
    "PASSWORD_TEXT": "password",
    "SPIN_BUTTON": "number",
}


def atk_role(role_name):
    """Map an ATK / AT-SPI Role *name* string (e.g. "PUSH_BUTTON") to a role in
    the descriptor model. Pure: no GTK/AT-SPI import, so it is unit-testable on
    any host.

    Note we deliberately return the RAW mapped role, NOT normalize_role(...).
    The map can yield a transient role (`progress`/`spinner`/`toast`/`tooltip`)
    or a value-role (`status`) that is not in the structural ROLES vocabulary;
    those must survive into the Node so the oracle can drop transients (rule 2)
    and fold value-roles (Layer 2). The oracle normalizes the role to `node` at
    descriptor-serialization time, exactly as for a hand-built Node with role
    "status"/"progress". An unmapped role -> `node`."""
    return ATSPI_ROLE_TO_ROLE.get(role_name, "node")


def _input_type(role_name, role):
    """Input `type` refinement for textfields, from the role name (e.g.
    PASSWORD_TEXT -> password). None when there is nothing to refine."""
    if role != "textfield":
        return None
    return INPUT_TYPE_BY_ROLE.get(role_name)


def _live_role(role_name, role, attrs):
    """Promote a status-bar / live-region accessible to the value-role `status`
    (docs/signature.md "Value-state"), so its changing value folds into the
    canonical signature. STATUS_BAR maps to `text` by default; a node that
    carries an active `live` / `container-live` attribute (!= "off") is an
    announcing live region. `status` normalizes to `node`, matching the oracle."""
    if role_name == "STATUS_BAR":
        return "status"
    if role in ("text", "node"):
        for k in ("live", "container-live", "container_live"):
            v = (attrs.get(k) or "").strip().lower()
            if v and v != "off":
                return "status"
    return role


def _promote_progressbar(role_name, role, has_value):
    """A PROGRESS_BAR is the transient `progress` by default (a loading
    indicator is dropped). A progress bar that publishes a readable value is a
    meaningful value-state surface, so promote it to the value-role `progressbar`
    (NOT transient; normalizes to `node`) when it has a value."""
    if role == "progress" and role_name == "PROGRESS_BAR" and has_value:
        return "progressbar"
    return role


def node_from_attrs(role_name, accessible_id=None, attrs=None, value=None):
    """Build ONE canonical Node (no children) from already-extracted accessible
    attributes. This is the single mapping both live walks funnel through, and
    it is PURE (no GTK/AT-SPI import) so the widget-tree -> descriptor mapping is
    unit-testable with synthetic inputs on any host.

    - role_name: the ATK / AT-SPI Role name string (e.g. "PUSH_BUTTON").
    - accessible_id: the stable developer id (accessible-id / buildable-id), or
      None / "".
    - attrs: the accessible's object-attribute dict (for the live-region check);
      may be None.
    - value: the raw displayed value string the accessible exposes, or None.

    The raw value is carried onto the Node as-is (the oracle's Node model allows
    a `value` on any node). Rule 1 is preserved NOT by nulling it here but by the
    oracle's value-gate: is_value_bearing requires the role to be a value-role OR
    the node to be value_node-flagged (Layer 3), so a chrome label/button with a
    stray value emits NOTHING in the descriptor or the V: section. Keeping the
    raw value means a Layer-3 `value_nodes:` selector can later flag a chrome
    node and have its already-captured value fold into the V: section, exactly
    like the runner's apply_value_nodes path.
    """
    attrs = attrs or {}
    role = atk_role(role_name)
    role = _live_role(role_name, role, attrs)
    role = _promote_progressbar(role_name, role, value is not None and str(value) != "")
    aid = (accessible_id or "").strip() or None
    return Node(
        role=role,
        id=aid,
        type=_input_type(role_name, role),
        icon=None,  # ATK / AT-SPI have no standard icon attribute; hook left open
        value=value,
    )


# =============================================================================
# GTK / ATK in-process walk (build_node_atk). Imports `gi` lazily.
# =============================================================================

def _atk_role_name(acc):
    """Resolve the ATK Role *name* string for an ATK accessible. The enum's
    value name is e.g. "ATK_ROLE_PUSH_BUTTON"; strip the prefix to
    "PUSH_BUTTON" so it keys ATSPI_ROLE_TO_ROLE."""
    try:
        role = acc.get_role()
    except Exception:
        return ""
    try:
        name = getattr(role, "value_name", None)
        if name:
            return name.replace("ATK_ROLE_", "").replace("ATSPI_ROLE_", "")
    except Exception:
        pass
    try:
        rn = acc.get_role_name() or ""
        return rn.strip().upper().replace(" ", "_").replace("-", "_")
    except Exception:
        return ""


def _atk_id(acc):
    """Stable developer id for an ATK accessible. GTK exposes the buildable name
    via the `accessible-id` object attribute (set by gtk_widget_set_name / the
    XML buildable id). Empty -> None."""
    try:
        attrs = _atk_attrs(acc)
        for k in ("accessible-id", "id", "html-id", "name"):
            v = (attrs.get(k) or "").strip()
            if v:
                return v
    except Exception:
        pass
    return None


def _atk_attrs(acc):
    """Object-attribute dict for an ATK accessible (atk_object_get_attributes
    returns a list of "key:value" AtkAttribute pairs). {} on failure."""
    out = {}
    try:
        for a in (acc.get_attributes() or []):
            # AtkAttribute is a struct with .name / .value.
            name = getattr(a, "name", None)
            val = getattr(a, "value", None)
            if name is not None:
                out[name] = val
    except Exception:
        pass
    return out


def _atk_value(acc, role_name, attrs):
    """The raw displayed value an ATK accessible exposes: the AtkValue interface
    (sliders / spin buttons / progress bars), the AtkText interface (entry
    contents), or a live-region's name. Read permissively here (the resolved
    role is not final yet, because the live-region / progressbar promotion runs
    later in node_from_attrs); node_from_attrs makes the FINAL gating decision
    and drops the value for chrome roles, so rule 1 is preserved centrally."""
    # AtkValue: sliders, spin buttons, progress bars.
    try:
        if hasattr(acc, "get_current_value"):
            cv = acc.get_current_value()
            if cv is not None:
                return _fmt_value(cv)
    except Exception:
        pass
    # AtkText: the typed contents of an entry / text field.
    try:
        if hasattr(acc, "get_text") and hasattr(acc, "get_character_count"):
            n = acc.get_character_count()
            txt = acc.get_text(0, n if (n and n >= 0) else -1)
            if txt is not None:
                return str(txt)
    except Exception:
        pass
    # A status bar / live region with no value or text interface carries its
    # NAME as the value (the announced text). Keyed on role_name + the live
    # attrs (the role is not yet promoted), so node_from_attrs can promote it to
    # `status` and keep the value; a non-live LABEL keeps role `text` and the
    # value is dropped centrally.
    if role_name == "STATUS_BAR" or _is_live_attrs(attrs):
        try:
            nm = acc.get_name()
            if nm:
                return str(nm)
        except Exception:
            pass
    return None


def _is_live_attrs(attrs):
    for k in ("live", "container-live", "container_live"):
        v = (attrs.get(k) or "").strip().lower()
        if v and v != "off":
            return True
    return False


def build_node_atk(acc, depth=0):
    """Walk a live ATK accessible (a GTK widget's `get_accessible()`) into a
    canonical Node tree. In-process: no AT-SPI bus round-trip. Funnels every
    accessible through node_from_attrs so a GTK widget folds into the same
    descriptor a Qt widget does via the AT-SPI walk."""
    role_name = _atk_role_name(acc)
    attrs = _atk_attrs(acc)
    raw_value = _atk_value(acc, role_name, attrs)
    node = node_from_attrs(role_name, _atk_id(acc), attrs, raw_value)
    if depth < MAX_DEPTH:
        try:
            for i in range(acc.get_n_accessible_children()):
                child = acc.ref_accessible_child(i)
                if child is not None:
                    node.children.append(build_node_atk(child, depth + 1))
        except Exception:
            pass
    return node


def _fmt_value(cv):
    """Render a numeric value into the strict period-decimal grammar value_class
    accepts: an integral value prints with no fraction (5.0 -> "5" -> POS1),
    otherwise the plain repr (locale-safe, period decimal)."""
    try:
        f = float(cv)
    except Exception:
        return str(cv)
    if f == int(f):
        return str(int(f))
    return repr(f)


# =============================================================================
# AT-SPI accessible walk (build_node_atspi). For Qt and any AT-SPI toolkit.
# This is the in-app twin of runners/linux-atspi.py's build_node, sharing the
# role map and node model so a Qt app folds identically to the runner's view.
# Imports `gi` / `Atspi` lazily.
# =============================================================================

def _atspi_role_name(acc):
    try:
        role = acc.get_role()
    except Exception:
        return ""
    try:
        name = getattr(role, "value_name", None)
        if name:
            return name.replace("ATSPI_ROLE_", "")
    except Exception:
        pass
    try:
        rn = acc.get_role_name() or ""
        return rn.strip().upper().replace(" ", "_").replace("-", "_")
    except Exception:
        return ""


def _atspi_id(acc):
    try:
        aid = (acc.get_accessible_id() or "").strip()
        if aid:
            return aid
    except Exception:
        pass
    return None


def _atspi_attrs(acc):
    try:
        return dict(acc.get_attributes() or {})
    except Exception:
        return {}


def _atspi_value(acc, role_name, attrs):
    """The raw displayed value an AT-SPI accessible exposes (Value iface, Text
    iface, or a live-region's name). Read permissively; node_from_attrs makes the
    final role-gated decision so chrome values are dropped (rule 1) centrally."""
    try:
        vi = acc.get_value_iface()
        if vi is not None:
            cv = vi.get_current_value()
            if cv is not None:
                return _fmt_value(cv)
    except Exception:
        pass
    try:
        ti = acc.get_text_iface()
        if ti is not None:
            n = ti.get_character_count()
            txt = ti.get_text(0, n if (n and n >= 0) else -1)
            if txt is not None:
                return str(txt)
    except Exception:
        pass
    if role_name == "STATUS_BAR" or _is_live_attrs(attrs):
        try:
            nm = acc.get_name()
            if nm:
                return str(nm)
        except Exception:
            pass
    return None


def build_node_atspi(acc, depth=0):
    """Walk a live AT-SPI accessible into a canonical Node tree. Funnels through
    node_from_attrs, identical to the GTK ATK walk, so a Qt app and a GTK app
    produce the same descriptor for the same logical screen."""
    role_name = _atspi_role_name(acc)
    attrs = _atspi_attrs(acc)
    raw_value = _atspi_value(acc, role_name, attrs)
    node = node_from_attrs(role_name, _atspi_id(acc), attrs, raw_value)
    if depth < MAX_DEPTH:
        try:
            for i in range(acc.get_child_count()):
                child = acc.get_child_at_index(i)
                if child is not None:
                    node.children.append(build_node_atspi(child, depth + 1))
        except Exception:
            pass
    return node


# =============================================================================
# Display-only labels + anchor (NEVER hashed). Used for `map show` only.
# =============================================================================

def label_of_atk(acc):
    """Display-only localized label for an ATK accessible (NEVER hashed)."""
    try:
        return (acc.get_name() or "").strip()
    except Exception:
        return ""


def anchor_of(acc):
    """Screen anchor = window/view identity, if available. There is no route on
    a desktop app, so use the stable id of the top accessible, else None. Mirrors
    runners/linux-atspi.py's _anchor_of for the in-app root window."""
    aid = _atk_id(acc)
    if aid:
        return aid
    return None
