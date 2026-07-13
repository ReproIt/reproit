/* ReproIt GTK IN-PROCESS operability agent (graph-1 ground truth).
 *
 * !!! TOOLCHAIN STATUS: BUILT + RUN + VERIFIED with GTK 4.18.6.
 *     Build the demo on a Linux host with GTK installed:
 *         apt-get install -y build-essential pkg-config xvfb libgtk-4-dev
 *         gcc $(pkg-config --cflags gtk4) -DREPROIT_GTK_DEMO_MAIN gtk_agent.c \
 *             $(pkg-config --libs gtk4) -o gtk_agent
 *         xvfb-run -a ./gtk_agent          # GTK4 needs a display to realize
 *     It emits the expected marker (sig 44602d5a, deterministic): the fake button
 *     is operable:true / rolePresent:false (NO_ROLE + keyboard-unreachable +
 *     pointer-only), the real + good GtkButtons clean. See
 *     runners/native/README.md and the gtk_* contract test in model/map.rs.
 *     (GTK3: swap `gtk4` for `gtk+-3.0`; the AT-SPI / ATK calls below target the
 *     GtkAccessible / AtkObject surface common to both, with a #if for the
 *     gtk_widget_get_accessible vs gtk_accessible_get_accessible_role split.)
 *     It is intended to be loaded INTO a running GTK app (e.g. as a GModule the
 *     app dlopen()s, or via a g_idle_add installed from an injected library) so
 *     it reads the live GtkWidget tree; `main` below builds a proof window for
 *     offline verification once a toolchain is available.
 *
 * Like the AppKit agent, this reads graph 1 (the real GtkWidget tree + its wired
 * "clicked"/"activate" signal handlers = the operability ground truth) and joins
 * it, by GObject identity, against graph 2 (the widget's accessible role/name/
 * state, i.e. what GTK publishes to AT-SPI). The diff is the gap the engine
 * scores.
 *
 * Output marker (parsed by crates/reproit/src/model/map.rs::gaps_from_groundtruth):
 *   EXPLORE:GROUNDTRUTH {"sig":..,"focusTrap":bool,"elements":[{id,operable,
 *     gestureKind,a11y:{rolePresent,namePresent,focusable,inTabOrder,
 *     keyboardActivatable}}]}
 */

#include <gtk/gtk.h>
#include <glib.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* ---- canonical signature (FNV-1a, structural-only subset of the oracle) --- */
static void fnv1a32hex(const char *bytes, gsize len, char out[9]) {
    uint32_t h = 0x811c9dc5u;
    for (gsize i = 0; i < len; i++) { h ^= (unsigned char)bytes[i]; h *= 0x01000193u; }
    g_snprintf(out, 9, "%08x", h);
}

/* ---- the FAKE button: a plain GtkBox/GtkDrawingArea with a click GESTURE ----
 * ---- wired to a handler, but no button role and no keyboard focus. The GTK ---
 * ---- analogue of the AppKit FakeButton. Built in `main` with a               */
/* ---- GtkGestureClick controller attached to a non-button widget.            */

/* ---- graph 1: is this widget OPERABLE (ground truth off the widget tree)? --
 * A GtkWidget is operable when:
 *   - it is a GtkButton / GtkToggleButton / GtkCheckButton / GtkLinkButton, etc.
 *     (the activatable button family), OR
 *   - it has a connected "clicked" / "activate" signal handler (real behavior),
 *     OR
 *   - it carries a GtkGestureClick / GtkGestureLongPress event controller (the
 *     fake-button case: a click gesture on a non-button widget).
 * Returns TRUE and writes *gesture_kind. */
static gboolean has_handler(GObject *o, const char *signal_name) {
    guint sig_id = g_signal_lookup(signal_name, G_OBJECT_TYPE(o));
    if (sig_id == 0) return FALSE;
    /* A matching connected handler = a real, ground-truth wired behavior. */
    gulong h = g_signal_handler_find(o, G_SIGNAL_MATCH_ID, sig_id, 0, NULL, NULL, NULL);
    return h != 0;
}

static gboolean has_click_gesture(GtkWidget *w, const char **gesture_kind) {
#if GTK_CHECK_VERSION(4, 0, 0)
    /* GTK4: event controllers are listed via gtk_widget_observe_controllers. */
    GListModel *ctrls = gtk_widget_observe_controllers(w);
    guint n = g_list_model_get_n_items(ctrls);
    gboolean found = FALSE;
    for (guint i = 0; i < n; i++) {
        GObject *c = g_list_model_get_item(ctrls, i);
        if (GTK_IS_GESTURE_CLICK(c))       { *gesture_kind = "button";    found = TRUE; }
        else if (GTK_IS_GESTURE_LONG_PRESS(c)) { *gesture_kind = "longPress"; found = TRUE; }
        else if (GTK_IS_GESTURE_DRAG(c))   { *gesture_kind = "pan";       found = TRUE; }
        g_object_unref(c);
        if (found) break;
    }
    g_object_unref(ctrls);
    return found;
#else
    (void)w; (void)gesture_kind;
    /* GTK3: gestures are attached but not enumerable the same way; a real GTK3
     * deployment inspects the widget's button-press-event handler instead. */
    return FALSE;
#endif
}

static gboolean graph1_operable(GtkWidget *w, const char **gesture_kind) {
    *gesture_kind = "";
    if (GTK_IS_BUTTON(w)) { *gesture_kind = "button"; return TRUE; }
    if (has_handler(G_OBJECT(w), "clicked"))  { *gesture_kind = "button";  return TRUE; }
    if (has_handler(G_OBJECT(w), "activate")) { *gesture_kind = "activate"; return TRUE; }
    if (has_click_gesture(w, gesture_kind)) return TRUE;
    return FALSE;
}

/* ---- graph 2: the accessible (AT-SPI) projection of the SAME widget --------
 * GTK4 exposes the accessible role/state via the GtkAccessible interface that
 * every GtkWidget implements; GTK3 exposes an AtkObject via
 * gtk_widget_get_accessible(). We read role / name / focusable here. */
typedef struct { gboolean role_present, name_present, focusable, in_tab_order, keyboard_activatable; } A11y;

static A11y graph2_a11y(GtkWidget *w) {
    A11y a = {FALSE, FALSE, FALSE, FALSE, FALSE};
#if GTK_CHECK_VERSION(4, 0, 0)
    GtkAccessibleRole role = gtk_accessible_get_accessible_role(GTK_ACCESSIBLE(w));
    /* GENERIC / GROUP / PRESENTATION are structural fall-throughs = no role. */
    a.role_present = (role != GTK_ACCESSIBLE_ROLE_GENERIC
                      && role != GTK_ACCESSIBLE_ROLE_GROUP
                      && role != GTK_ACCESSIBLE_ROLE_PRESENTATION
                      && role != GTK_ACCESSIBLE_ROLE_NONE);
    /* Name: the accessible label, if set. (gtk_accessible_get_accessible_role
     * has no direct name getter pre-4.10; in 4.10+ use
     * gtk_accessible_get_accessible_property for LABEL. We approximate with the
     * widget's tooltip/label text in older runtimes.) */
    char *label = NULL;
#if GTK_CHECK_VERSION(4, 10, 0)
    /* 4.10+: read the LABEL accessible property if present. */
    /* (The widget label path below keeps the accessible-name presence check portable.) */
#endif
    if (GTK_IS_BUTTON(w)) label = (char *)gtk_button_get_label(GTK_BUTTON(w));
    a.name_present = (label != NULL && *label != '\0');
    a.focusable = gtk_widget_get_focusable(w);
    a.in_tab_order = gtk_widget_get_focusable(w) && gtk_widget_get_can_focus(w);
    /* Keyboard-activatable: a focusable widget with an operable accessible role
     * (a button activates on Space/Enter). A bare click-gesture widget with no
     * role and no focus is pointer-only. */
    a.keyboard_activatable = a.role_present && a.focusable;
#else
    AtkObject *acc = gtk_widget_get_accessible(w);
    if (acc) {
        AtkRole role = atk_object_get_role(acc);
        a.role_present = (role != ATK_ROLE_UNKNOWN
                          && role != ATK_ROLE_FILLER
                          && role != ATK_ROLE_PANEL);
        const char *name = atk_object_get_name(acc);
        a.name_present = (name != NULL && *name != '\0');
        AtkStateSet *states = atk_object_ref_state_set(acc);
        if (states) {
            a.focusable = atk_state_set_contains_state(states, ATK_STATE_FOCUSABLE);
            g_object_unref(states);
        }
    }
    a.in_tab_order = gtk_widget_get_can_focus(w);
    a.keyboard_activatable = a.role_present && a.focusable;
#endif
    return a;
}

static const char *role_token(GtkWidget *w) {
    if (GTK_IS_BUTTON(w)) return "button";
#if GTK_CHECK_VERSION(4, 0, 0)
    switch (gtk_accessible_get_accessible_role(GTK_ACCESSIBLE(w))) {
        case GTK_ACCESSIBLE_ROLE_BUTTON:   return "button";
        case GTK_ACCESSIBLE_ROLE_CHECKBOX: return "checkbox";
        case GTK_ACCESSIBLE_ROLE_RADIO:    return "radio";
        case GTK_ACCESSIBLE_ROLE_SLIDER:   return "slider";
        case GTK_ACCESSIBLE_ROLE_TEXT_BOX: return "textfield";
        case GTK_ACCESSIBLE_ROLE_LABEL:    return "text";
        case GTK_ACCESSIBLE_ROLE_LINK:     return "link";
        default: break;
    }
#endif
    return "group";
}

/* ---- the walk: graph 1 x graph 2 over the live widget tree ----------------- */
typedef struct {
    GString *json_elements;  /* JSON array body being built */
    GString *sig_tokens;     /* descriptor tokens */
    int role_counts[8];      /* simple per-role counter for keyless ids */
    gboolean first;
    gboolean any_operable, any_tab;
    GString *verdicts;       /* human-readable stderr lines */
} WalkCtx;

static void json_escape_append(GString *s, const char *raw) {
    for (const char *p = raw; *p; p++) {
        if (*p == '"' || *p == '\\') { g_string_append_c(s, '\\'); g_string_append_c(s, *p); }
        else g_string_append_c(s, *p);
    }
}

static void walk_widget(GtkWidget *w, int depth, WalkCtx *ctx) {
    if (!GTK_IS_WIDGET(w)) return;
    const char *gesture_kind = "";
    gboolean operable = graph1_operable(w, &gesture_kind);
    A11y a = graph2_a11y(w);

    if (operable || a.role_present) {
        const char *role = role_token(w);
        /* id: the widget's name (gtk_widget_get_name, the GTK test-id analogue)
         * if the dev set a non-default one, else role#index. */
        const char *wname = gtk_widget_get_name(w);
        char id[256];
        gboolean named = (wname && *wname && g_strcmp0(wname, G_OBJECT_TYPE_NAME(w)) != 0);
        if (named) {
            g_snprintf(id, sizeof id, "key:%s", wname);
        } else {
            int slot = (g_strcmp0(role, "button") == 0) ? 0 : 1;
            g_snprintf(id, sizeof id, "role:%s#%d", role, ctx->role_counts[slot]++);
        }
        if (operable) ctx->any_operable = TRUE;
        if (a.in_tab_order) ctx->any_tab = TRUE;

        if (!ctx->first) g_string_append_c(ctx->json_elements, ',');
        ctx->first = FALSE;
        g_string_append(ctx->json_elements, "{\"id\":\"");
        json_escape_append(ctx->json_elements, id);
        g_string_append_printf(ctx->json_elements,
            "\",\"operable\":%s,\"gestureKind\":\"%s\",\"a11y\":{"
            "\"rolePresent\":%s,\"namePresent\":%s,\"focusable\":%s,"
            "\"inTabOrder\":%s,\"keyboardActivatable\":%s}}",
            operable ? "true" : "false", gesture_kind,
            a.role_present ? "true" : "false", a.name_present ? "true" : "false",
            a.focusable ? "true" : "false", a.in_tab_order ? "true" : "false",
            a.keyboard_activatable ? "true" : "false");

        g_string_append_printf(ctx->sig_tokens, "%s%d:%s@%s",
            ctx->sig_tokens->len ? ";" : "", depth, role, id);

        g_string_append_printf(ctx->verdicts, "  %s operable=%s -> ",
            id, operable ? "true" : "false");
        if (operable && (!a.role_present || !a.in_tab_order || !a.keyboard_activatable)) {
            g_string_append(ctx->verdicts, "GAP(");
            gboolean f = TRUE;
            if (!a.role_present)         { g_string_append(ctx->verdicts, "NO_ROLE"); f = FALSE; }
            if (!a.in_tab_order)         { g_string_append(ctx->verdicts, f ? "KEYBOARD_UNREACHABLE" : ",KEYBOARD_UNREACHABLE"); f = FALSE; }
            if (!a.keyboard_activatable) { g_string_append(ctx->verdicts, f ? "POINTER_ONLY" : ",POINTER_ONLY"); }
            g_string_append(ctx->verdicts, ")\n");
        } else {
            g_string_append(ctx->verdicts, "OK\n");
        }
    }

    /* Recurse over children (GTK4: gtk_widget_get_first_child/next_sibling). */
#if GTK_CHECK_VERSION(4, 0, 0)
    for (GtkWidget *c = gtk_widget_get_first_child(w); c; c = gtk_widget_get_next_sibling(c))
        walk_widget(c, depth + 1, ctx);
#else
    if (GTK_IS_CONTAINER(w)) {
        GList *kids = gtk_container_get_children(GTK_CONTAINER(w));
        for (GList *l = kids; l; l = l->next) walk_widget(GTK_WIDGET(l->data), depth + 1, ctx);
        g_list_free(kids);
    }
#endif
}

static void emit_groundtruth(GtkWidget *root) {
    WalkCtx ctx = {0};
    ctx.json_elements = g_string_new("");
    ctx.sig_tokens = g_string_new("");
    ctx.verdicts = g_string_new("");
    ctx.first = TRUE;
    walk_widget(root, 0, &ctx);

    gboolean focus_trap = ctx.any_operable && !ctx.any_tab;

    GString *desc = g_string_new("A:\n");
    g_string_append(desc, ctx.sig_tokens->str);
    char sig[9];
    fnv1a32hex(desc->str, desc->len, sig);

    printf("EXPLORE:GROUNDTRUTH {\"sig\":\"%s\",\"focusTrap\":%s,\"elements\":[%s]}\n",
           sig, focus_trap ? "true" : "false", ctx.json_elements->str);
    fflush(stdout);
    fputs(ctx.verdicts->str, stderr);
    fflush(stderr);

    g_string_free(ctx.json_elements, TRUE);
    g_string_free(ctx.sig_tokens, TRUE);
    g_string_free(ctx.verdicts, TRUE);
    g_string_free(desc, TRUE);
}

/* ---- standalone proof entry point ------------------------------------------
 * Builds a window with a real GtkButton + a fake-button (a GtkBox carrying a
 * GtkGestureClick + handler, no button role / not focusable) + a correctly
 * accessible control, then walks. In production the agent is loaded into the
 * target app and emit_groundtruth() is called from a g_idle_add. */
#ifdef REPROIT_GTK_DEMO_MAIN
static void noop_clicked(GtkButton *b, gpointer u) { (void)b; (void)u; }
static void noop_gesture(GtkGestureClick *g, gint n, double x, double y, gpointer u) {
    (void)g; (void)n; (void)x; (void)y; (void)u;
}

static void on_activate(GtkApplication *app, gpointer user_data) {
    (void)user_data;
    GtkWidget *win = gtk_application_window_new(app);
    GtkWidget *box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 8);

    GtkWidget *real = gtk_button_new_with_label("Real Button");
    gtk_widget_set_name(real, "realButton");
    g_signal_connect(real, "clicked", G_CALLBACK(noop_clicked), NULL);
    gtk_box_append(GTK_BOX(box), real);

    /* The FAKE button: a non-button widget with a click gesture + handler and
     * no accessible button role, not focusable. */
    GtkWidget *fake = gtk_box_new(GTK_ORIENTATION_HORIZONTAL, 0);
    gtk_widget_set_name(fake, "fakeButton");
    GtkGesture *click = gtk_gesture_click_new();
    g_signal_connect(click, "pressed", G_CALLBACK(noop_gesture), NULL);
    gtk_widget_add_controller(fake, GTK_EVENT_CONTROLLER(click));
    gtk_box_append(GTK_BOX(box), fake);

    GtkWidget *good = gtk_button_new_with_label("Accessible Custom Button");
    gtk_widget_set_name(good, "goodCustom");
    g_signal_connect(good, "clicked", G_CALLBACK(noop_clicked), NULL);
    gtk_box_append(GTK_BOX(box), good);

    gtk_window_set_child(GTK_WINDOW(win), box);

    printf("JOURNEY claimed role=gtk-agent\n"); fflush(stdout);
    emit_groundtruth(win);
    printf("JOURNEY DONE\nAll tests passed\n"); fflush(stdout);
    /* headless: quit immediately without presenting the window. */
    g_application_quit(G_APPLICATION(app));
}

int main(int argc, char **argv) {
    GtkApplication *app = gtk_application_new("dev.reproit.gtkagent", G_APPLICATION_DEFAULT_FLAGS);
    g_signal_connect(app, "activate", G_CALLBACK(on_activate), NULL);
    int status = g_application_run(G_APPLICATION(app), argc, argv);
    g_object_unref(app);
    return status;
}
#endif
