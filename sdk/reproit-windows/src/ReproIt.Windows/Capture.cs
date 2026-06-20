// ReproIt Windows, live visual-tree capture for WPF (System.Windows) and
// WinUI 3 (Microsoft.UI.Xaml).
//
// This file maps a live XAML element tree into the canonical ReproIt.Core.Node
// tree the structural signature hashes (docs/signature.md "Inputs"). It deals
// with BOTH XAML stacks at once through reflection on the shared shape they
// expose (a "Children"/"Content"/"Items" visual nesting, AutomationProperties
// for Name / AutomationId, x:Name, control class names, and the editable Value),
// so the one assembly works whether the host app references WPF or WinUI 3
// without a hard compile-time dependency on both. Roles are derived from the
// control class / automation control type, NEVER from the (localized) text, so
// the signature is byte-identical to the Rust oracle and the windows-uia.py
// runner that drives the same app over UI Automation.
//
// The control-class -> role map and the value/type rules mirror
// runners/windows-uia.py (UIA_CONTROLTYPE_TO_ROLE and _uia_role_live), so the
// in-app SDK and the external UIA runner compute the SAME signature for the same
// screen. The actual walk is reflection-based and never touches a type that may
// be absent, so it degrades gracefully on either framework.

using System;
using System.Collections;
using System.Collections.Generic;
using System.Globalization;
using System.Reflection;
using ReproIt.Core;

namespace ReproIt.Windows
{
    /// <summary>Reflection-driven XAML visual-tree walker shared by WPF and WinUI 3.
    /// All access to framework types goes through reflection so this assembly never
    /// hard-links System.Windows or Microsoft.UI.Xaml; it reads whichever is present
    /// at runtime.</summary>
    internal static class Capture
    {
        // Stable attached-property style markers a developer can set via the public
        // ReproItClient.Tag* helpers (stored in a side table keyed by the element
        // identity), so the capture can honor explicit id / icon / transient /
        // value-node annotations without an attached DependencyProperty per stack.
        internal sealed class Marks
        {
            public string Id;
            public string Icon;
            public bool Transient;
            public bool ValueNode;
            public string Value; // explicit value override for a value-node mark
        }

        // A conditional-weak table keyed by object identity, so marks do not keep
        // elements alive and survive across captures.
        private static readonly System.Runtime.CompilerServices.ConditionalWeakTable<object, Marks> MarkTable =
            new System.Runtime.CompilerServices.ConditionalWeakTable<object, Marks>();

        internal static Marks MarksFor(object element, bool create)
        {
            if (element == null)
            {
                return null;
            }
            Marks m;
            if (MarkTable.TryGetValue(element, out m))
            {
                return m;
            }
            if (!create)
            {
                return null;
            }
            m = new Marks();
            MarkTable.Add(element, m);
            return m;
        }

        /// <summary>Build the canonical Node tree rooted at the given content root
        /// (a WPF Window's Content or a WinUI Window's Content / a page root). The
        /// root node is forced to "screen", mirroring the other SDKs and the runner's
        /// WindowControl -> screen mapping.</summary>
        public static Node CaptureTree(object root)
        {
            var children = new List<Node>();
            foreach (var child in VisualChildren(root))
            {
                var node = BuildNode(child);
                if (node != null)
                {
                    children.Add(node);
                }
            }
            var screen = new Node("screen");
            screen.Children = children;
            return screen;
        }

        /// <summary>Map one visible element to a canonical node (with its visible
        /// subtree), or null if it is not visible. Invisible/zero-area wrappers are
        /// skipped but their visible descendants are hoisted, so the structure is
        /// independent of non-rendering wrappers (same rule as the Android SDK).</summary>
        private static Node BuildNode(object element)
        {
            if (element == null || !IsVisible(element))
            {
                return null;
            }
            string role = RoleOf(element);
            var children = new List<Node>();
            foreach (var child in VisualChildren(element))
            {
                var node = BuildNode(child);
                if (node != null)
                {
                    children.Add(node);
                }
            }
            string value = ValueOf(element, role);
            var n = new Node(role)
            {
                Id = IdOf(element),
                Type = TypeOf(element, role),
                Icon = IconOf(element),
                Transient = IsTransient(element, role),
                Value = value,
                // The oracle only consults ValueNode when a value is present; flag any
                // element that supplied a value but whose canonical role is not a
                // structural value-role (sliders/progress/status/opt-in text), so the
                // value-class enters the V: section. A textfield's role IS a
                // value-role, so it needs no flag.
                ValueNode = value != null && role != "textfield",
                Children = children,
            };
            return n;
        }

        // ---- role mapping (mirrors runners/windows-uia.py UIA_CONTROLTYPE_TO_ROLE) ----

        // Class-name (and base-class-name) -> canonical role. Matched against the
        // element's runtime type name and its base chain so framework-specific
        // subclasses (e.g. a custom Button) still resolve. Ordering is most-specific
        // first via an explicit probe in RoleOf.
        private static readonly Dictionary<string, string> ClassToRole = new Dictionary<string, string>(StringComparer.Ordinal)
        {
            // text input
            { "TextBox", "textfield" },
            { "RichTextBox", "textfield" },
            { "PasswordBox", "textfield" },
            { "AutoSuggestBox", "textfield" },
            { "ComboBox", "textfield" },
            { "SearchBox", "textfield" },
            // buttons
            { "Button", "button" },
            { "RepeatButton", "button" },
            { "HyperlinkButton", "link" },
            { "DropDownButton", "button" },
            { "SplitButton", "button" },
            { "ToggleButton", "checkbox" },
            { "AppBarButton", "button" },
            // toggles / selection
            { "CheckBox", "checkbox" },
            { "RadioButton", "radio" },
            { "ToggleSwitch", "switch" },
            // sliders / progress
            { "Slider", "slider" },
            { "ProgressBar", "progress" },   // transient unless it carries a value
            { "ProgressRing", "progress" },  // transient
            { "RatingControl", "slider" },
            // text / headers
            { "TextBlock", "text" },
            { "RichTextBlock", "text" },
            { "Label", "text" },
            // image
            { "Image", "image" },
            { "ImageIcon", "image" },
            // lists
            { "ListView", "list" },
            { "ListBox", "list" },
            { "GridView", "list" },
            { "DataGrid", "list" },
            { "TreeView", "list" },
            { "ItemsControl", "list" },
            { "ListViewItem", "listitem" },
            { "ListBoxItem", "listitem" },
            { "GridViewItem", "listitem" },
            { "TreeViewItem", "listitem" },
            { "DataGridRow", "listitem" },
            // tabs
            { "TabControl", "tab" },
            { "TabView", "tab" },
            { "TabItem", "tab" },
            { "TabViewItem", "tab" },
            { "Pivot", "tab" },
            { "PivotItem", "tab" },
            // menus
            { "Menu", "menu" },
            { "MenuBar", "menu" },
            { "MenuFlyout", "menu" },
            { "ContextMenu", "menu" },
            { "MenuItem", "menuitem" },
            { "MenuBarItem", "menuitem" },
            { "MenuFlyoutItem", "menuitem" },
            // dialogs
            { "ContentDialog", "dialog" },
            { "Window", "dialog" },
            { "Popup", "dialog" },
            // transient chrome
            { "ToolTip", "tooltip" },
            { "InfoBadge", "badge" },
            // containers -> group
            { "Grid", "group" },
            { "StackPanel", "group" },
            { "Canvas", "group" },
            { "Border", "group" },
            { "Panel", "group" },
            { "DockPanel", "group" },
            { "WrapPanel", "group" },
            { "ScrollViewer", "group" },
            { "Expander", "group" },
            { "GroupBox", "group" },
            { "UserControl", "group" },
            { "ContentControl", "group" },
            { "ContentPresenter", "group" },
            { "ToolBar", "group" },
            { "StatusBar", "text" },
        };

        /// <summary>The canonical role for an element, from its class / automation
        /// control type only, never from text. An explicit AutomationProperties
        /// control-type or a switch-style toggle is honored; otherwise the class chain
        /// is matched. Anything outside the vocabulary normalizes to "node".</summary>
        private static string RoleOf(object element)
        {
            // Walk the runtime type's base chain so a subclass of Button still maps.
            Type t = element.GetType();
            while (t != null && t != typeof(object))
            {
                string role;
                if (ClassToRole.TryGetValue(t.Name, out role))
                {
                    // Promote a ToggleButton/CheckBox that localizes itself as a switch
                    // to "switch" (mirrors the runner's checkbox->switch promotion).
                    if ((role == "checkbox") && LooksLikeSwitch(element))
                    {
                        return "switch";
                    }
                    return role;
                }
                t = t.BaseType;
            }
            // A header is a TextBlock/Label the developer marked as a heading via
            // automation (AutomationProperties.HeadingLevel != None).
            if (IsHeading(element))
            {
                return "header";
            }
            return "node";
        }

        private static bool LooksLikeSwitch(object element)
        {
            string n = element.GetType().Name;
            return n.IndexOf("Switch", StringComparison.OrdinalIgnoreCase) >= 0 ||
                   n.IndexOf("Toggle", StringComparison.OrdinalIgnoreCase) >= 0;
        }

        // ---- id ----------------------------------------------------------------

        /// <summary>Stable developer id: an explicit ReproItClient.TagId mark, else
        /// AutomationProperties.AutomationId, else the element's x:Name. Empty/auto
        /// ids are omitted.</summary>
        private static string IdOf(object element)
        {
            var m = MarksFor(element, false);
            if (m != null && !string.IsNullOrEmpty(m.Id))
            {
                return m.Id;
            }
            string aid = GetAutomationString(element, "AutomationIdProperty", "GetAutomationId");
            if (!string.IsNullOrEmpty(aid))
            {
                return aid.Trim();
            }
            string name = GetStringProperty(element, "Name");
            if (!string.IsNullOrEmpty(name))
            {
                return name.Trim();
            }
            return null;
        }

        // ---- type refinement (textfield only) -----------------------------------

        /// <summary>The optional input-type refinement for a textfield node. A
        /// PasswordBox is "password"; a ComboBox is "text"; otherwise we map common
        /// hints (email/number) from the input scope where present, defaulting to
        /// "text". Null for non-textfield roles. Mirrors the runner's edit-type rule.</summary>
        private static string TypeOf(object element, string role)
        {
            if (role != "textfield")
            {
                return null;
            }
            string cls = ClassChainNames(element);
            if (cls.Contains("PasswordBox"))
            {
                return "password";
            }
            // InputScope (WPF/WinUI) hints; read by name to stay framework-agnostic.
            string scope = InputScopeName(element);
            if (!string.IsNullOrEmpty(scope))
            {
                string s = scope.ToLowerInvariant();
                if (s.Contains("password"))
                {
                    return "password";
                }
                if (s.Contains("email"))
                {
                    return "email";
                }
                if (s.Contains("number") || s.Contains("digits") || s.Contains("telephone"))
                {
                    return "number";
                }
            }
            return "text";
        }

        // ---- icon ---------------------------------------------------------------

        /// <summary>A language-independent icon identity, from an explicit
        /// ReproItClient.TagIcon mark or a FontIcon/SymbolIcon Glyph/Symbol where the
        /// content is an icon. Never derived from text.</summary>
        private static string IconOf(object element)
        {
            var m = MarksFor(element, false);
            if (m != null && !string.IsNullOrEmpty(m.Icon))
            {
                return m.Icon;
            }
            // A FontIcon exposes a Glyph string; a SymbolIcon exposes a Symbol enum.
            string glyph = GetStringProperty(element, "Glyph");
            if (!string.IsNullOrEmpty(glyph))
            {
                return glyph.Trim();
            }
            object sym = GetProperty(element, "Symbol");
            if (sym != null && sym.GetType().IsEnum)
            {
                return sym.ToString();
            }
            return null;
        }

        // ---- value (Layer 2) ----------------------------------------------------

        /// <summary>The displayed VALUE of a value-bearing element (docs/signature.md
        /// "Value-state", Layer 2), or null when the element bears no value. Detected
        /// from class / automation only, NEVER from chrome label text. Sources, in
        /// order: an explicit TagValue mark; a text field's entered Text; a range
        /// control's (Slider / ProgressBar) numeric Value; a live-region element's
        /// current Text (a status value-role). Chrome elements return null here.</summary>
        private static string ValueOf(object element, string role)
        {
            var m = MarksFor(element, false);
            if (m != null && m.ValueNode)
            {
                return m.Value ?? ReadText(element) ?? string.Empty;
            }

            // A text field's entered text (its role is the textfield value-role).
            if (role == "textfield")
            {
                // PasswordBox text is intentionally NOT read (never read secret entry).
                if (ClassChainNames(element).Contains("PasswordBox"))
                {
                    return null;
                }
                return GetStringProperty(element, "Text") ?? string.Empty;
            }

            // A range control (Slider / ProgressBar with a value): read Value.
            if (role == "slider" || role == "progress")
            {
                object v = GetProperty(element, "Value");
                if (v is double dv)
                {
                    return FormatRange(dv);
                }
            }

            // A live region announces status changes; treat its current text as a
            // status value-role (mirrors the runner's text->status promotion).
            if (role == "text" && IsLiveRegion(element))
            {
                return ReadText(element) ?? string.Empty;
            }
            return null;
        }

        /// <summary>Render a range value into the strict period-decimal grammar so a
        /// whole-number value classifies through it (e.g. 5 -> "5" -> POS1) and a
        /// fractional one keeps its period decimal. InvariantCulture pins the period.</summary>
        private static string FormatRange(double v)
        {
            if (double.IsInfinity(v) || double.IsNaN(v))
            {
                return v.ToString(CultureInfo.InvariantCulture);
            }
            if (v == Math.Floor(v))
            {
                return ((long)v).ToString(CultureInfo.InvariantCulture);
            }
            return v.ToString("R", CultureInfo.InvariantCulture);
        }

        // ---- transient ----------------------------------------------------------

        /// <summary>Transient detection (rule 2): a progress/spinner/tooltip/badge
        /// role, OR an explicit ReproItClient.TagTransient mark. A ProgressBar that
        /// publishes a value is promoted out of transient (it is a value surface, like
        /// the runner's progressbar promotion).</summary>
        private static bool IsTransient(object element, string role)
        {
            var m = MarksFor(element, false);
            if (m != null && m.Transient)
            {
                return true;
            }
            if (role == "progress")
            {
                // A progress control carrying a readable value is a value surface, not
                // a transient spinner: keep it (it will normalize to node in the body
                // and carry a value-class). Matches windows-uia.py's progressbar rule.
                object v = GetProperty(element, "Value");
                if (v is double)
                {
                    return false;
                }
                return true;
            }
            if (role == "tooltip" || role == "badge")
            {
                return true;
            }
            // A TeachingTip is a transient callout (snackbar/toast analogue): drop it.
            string cls = ClassChainNames(element);
            if (cls.Contains("TeachingTip"))
            {
                return true;
            }
            return false;
        }

        // ---- visibility + children ----------------------------------------------

        /// <summary>True when the element renders: Visibility == Visible (the enum
        /// member name) and a non-zero ActualWidth/Height where exposed.</summary>
        private static bool IsVisible(object element)
        {
            object vis = GetProperty(element, "Visibility");
            if (vis != null && string.Equals(vis.ToString(), "Collapsed", StringComparison.Ordinal))
            {
                return false;
            }
            if (vis != null && string.Equals(vis.ToString(), "Hidden", StringComparison.Ordinal))
            {
                return false;
            }
            // Zero-area elements do not render; skip them (their visible children are
            // hoisted by the caller since we still recurse from the parent).
            object w = GetProperty(element, "ActualWidth");
            object h = GetProperty(element, "ActualHeight");
            if (w is double dw && h is double dh && (dw <= 0.0 || dh <= 0.0))
            {
                // Allow zero-area pure containers through only if they have children
                // (a not-yet-measured panel); a zero-area leaf is invisible.
                if (CountChildren(element) == 0)
                {
                    return false;
                }
            }
            return true;
        }

        /// <summary>Enumerate an element's visual/content children in document order,
        /// covering the three shapes XAML uses: a Panel/ItemsControl "Children"/"Items"
        /// collection, a ContentControl "Content", and a decorator "Child". Reflection
        /// keeps this framework-agnostic.</summary>
        private static IEnumerable<object> VisualChildren(object element)
        {
            if (element == null)
            {
                yield break;
            }
            // 1. Panels (Grid/StackPanel/Canvas/...) expose a "Children" UIElementCollection.
            object children = GetProperty(element, "Children");
            if (children is IEnumerable childEnum && !(children is string))
            {
                foreach (var c in childEnum)
                {
                    if (c != null)
                    {
                        yield return c;
                    }
                }
                yield break;
            }
            // 2. ItemsControls (ListView/ComboBox/...) expose "Items".
            object items = GetProperty(element, "Items");
            if (items is IEnumerable itemEnum && !(items is string))
            {
                foreach (var c in itemEnum)
                {
                    if (c != null)
                    {
                        yield return c;
                    }
                }
                yield break;
            }
            // 3. A ContentControl exposes a single "Content".
            object content = GetProperty(element, "Content");
            if (content != null && !(content is string) && IsXamlElement(content))
            {
                yield return content;
                yield break;
            }
            // 4. A decorator (Border) exposes a single "Child".
            object child = GetProperty(element, "Child");
            if (child != null && IsXamlElement(child))
            {
                yield return child;
            }
        }

        private static int CountChildren(object element)
        {
            int n = 0;
            foreach (var _ in VisualChildren(element))
            {
                n++;
            }
            return n;
        }

        private static bool IsXamlElement(object o)
        {
            if (o == null)
            {
                return false;
            }
            // A XAML element derives (transitively) from DependencyObject in both
            // stacks; checking the base-chain name keeps us framework-agnostic.
            Type t = o.GetType();
            while (t != null && t != typeof(object))
            {
                if (t.Name == "DependencyObject" || t.Name == "UIElement" || t.Name == "FrameworkElement")
                {
                    return true;
                }
                t = t.BaseType;
            }
            return false;
        }

        // ---- display label (for the runner-parity labels field, not the hash) ----

        /// <summary>The accessible name of an element for the display-only label set:
        /// AutomationProperties.Name, else a TextBlock/Label/Button text content.
        /// Never folded into the hash.</summary>
        public static string NameOf(object element)
        {
            string an = GetAutomationString(element, "NameProperty", "GetName");
            if (!string.IsNullOrEmpty(an))
            {
                return an;
            }
            string text = ReadText(element);
            return text ?? string.Empty;
        }

        /// <summary>True if an element is invokable/clickable (a Button/Hyperlink or
        /// anything with an automation Invoke/Toggle pattern by class), for the
        /// display label set's unlabeled-tappable count.</summary>
        public static bool IsTappable(object element)
        {
            string cls = ClassChainNames(element);
            return cls.Contains("Button") || cls.Contains("Hyperlink") ||
                   cls.Contains("MenuItem") || cls.Contains("ListViewItem") ||
                   cls.Contains("TabItem") || cls.Contains("CheckBox") ||
                   cls.Contains("RadioButton") || cls.Contains("ToggleSwitch");
        }

        /// <summary>Walk the visual tree collecting RawNodes (name + tappable) in
        /// pre-order for the display-only label set.</summary>
        public static void WalkLabels(object root, List<RawNode> outNodes)
        {
            if (root == null || !IsVisible(root))
            {
                return;
            }
            string name = NameOf(root);
            bool tappable = IsTappable(root);
            if (name.Length > 0 || tappable)
            {
                outNodes.Add(new RawNode(name, tappable));
            }
            foreach (var c in VisualChildren(root))
            {
                WalkLabels(c, outNodes);
            }
        }

        // ---- reflection helpers --------------------------------------------------

        private static object GetProperty(object element, string name)
        {
            if (element == null)
            {
                return null;
            }
            try
            {
                PropertyInfo pi = element.GetType().GetProperty(name, BindingFlags.Public | BindingFlags.Instance);
                if (pi == null || !pi.CanRead)
                {
                    return null;
                }
                return pi.GetValue(element);
            }
            catch
            {
                return null;
            }
        }

        private static string GetStringProperty(object element, string name)
        {
            object v = GetProperty(element, name);
            return v as string;
        }

        /// <summary>Read an AutomationProperties attached value. Both WPF and WinUI 3
        /// expose a static AutomationProperties type with GetName/GetAutomationId
        /// accessors; we locate the type by name in the loaded assemblies and invoke
        /// the accessor reflectively so this works on either stack.</summary>
        private static string GetAutomationString(object element, string propertyFieldName, string getterName)
        {
            try
            {
                Type apType = FindAutomationPropertiesType(element);
                if (apType == null)
                {
                    return null;
                }
                MethodInfo getter = apType.GetMethod(getterName, BindingFlags.Public | BindingFlags.Static);
                if (getter == null)
                {
                    return null;
                }
                object result = getter.Invoke(null, new[] { element });
                return result as string;
            }
            catch
            {
                return null;
            }
        }

        private static Type _automationPropertiesType;
        private static bool _automationLookupDone;

        private static Type FindAutomationPropertiesType(object element)
        {
            if (_automationLookupDone)
            {
                return _automationPropertiesType;
            }
            _automationLookupDone = true;
            // WPF: System.Windows.Automation.AutomationProperties.
            // WinUI 3: Microsoft.UI.Xaml.Automation.AutomationProperties.
            string[] candidates =
            {
                "System.Windows.Automation.AutomationProperties, PresentationCore",
                "Microsoft.UI.Xaml.Automation.AutomationProperties, Microsoft.WinUI",
            };
            foreach (var name in candidates)
            {
                Type t = Type.GetType(name, false);
                if (t != null)
                {
                    _automationPropertiesType = t;
                    return t;
                }
            }
            // Fallback: scan the element's assembly + loaded assemblies for the type.
            foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
            {
                try
                {
                    Type t = asm.GetType("System.Windows.Automation.AutomationProperties", false)
                             ?? asm.GetType("Microsoft.UI.Xaml.Automation.AutomationProperties", false);
                    if (t != null)
                    {
                        _automationPropertiesType = t;
                        return t;
                    }
                }
                catch
                {
                    // ignore assemblies that fail to introspect
                }
            }
            return null;
        }

        private static bool IsHeading(object element)
        {
            try
            {
                Type apType = FindAutomationPropertiesType(element);
                if (apType == null)
                {
                    return false;
                }
                MethodInfo getter = apType.GetMethod("GetHeadingLevel", BindingFlags.Public | BindingFlags.Static);
                if (getter == null)
                {
                    return false;
                }
                object level = getter.Invoke(null, new[] { element });
                // The enum member "None" means not a heading; any other level is one.
                return level != null && !string.Equals(level.ToString(), "None", StringComparison.Ordinal);
            }
            catch
            {
                return false;
            }
        }

        private static bool IsLiveRegion(object element)
        {
            try
            {
                Type apType = FindAutomationPropertiesType(element);
                if (apType == null)
                {
                    return false;
                }
                MethodInfo getter = apType.GetMethod("GetLiveSetting", BindingFlags.Public | BindingFlags.Static);
                if (getter == null)
                {
                    return false;
                }
                object setting = getter.Invoke(null, new[] { element });
                // AutomationLiveSetting.Off means no live region; Polite/Assertive do.
                return setting != null && !string.Equals(setting.ToString(), "Off", StringComparison.Ordinal);
            }
            catch
            {
                return false;
            }
        }

        private static string InputScopeName(object element)
        {
            object scope = GetProperty(element, "InputScope");
            if (scope == null)
            {
                return null;
            }
            // Best-effort: stringify the scope (the enum/name typically renders usefully).
            return scope.ToString();
        }

        /// <summary>The element's own readable text (Text / Content-as-string), used
        /// only for value capture (live regions, opt-in marks) and the display label
        /// set, never for the structural hash.</summary>
        private static string ReadText(object element)
        {
            string text = GetStringProperty(element, "Text");
            if (text != null)
            {
                return text;
            }
            object content = GetProperty(element, "Content");
            if (content is string cs)
            {
                return cs;
            }
            return null;
        }

        private static string ClassChainNames(object element)
        {
            var sb = new System.Text.StringBuilder();
            Type t = element.GetType();
            while (t != null && t != typeof(object))
            {
                sb.Append(t.Name);
                sb.Append('|');
                t = t.BaseType;
            }
            return sb.ToString();
        }
    }
}
