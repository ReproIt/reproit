// In-process WPF operability agent: the WPF instance of reproit's two-graph
// operability diff (docs/operability-graph.md, "In-process native agent").
//
// WHY in-process: externally, the Windows a11y surface (UIAutomation) is the
// ONLY thing a driver can see, so a control that is operable by mouse but has
// no automation peer / no Button role is simply invisible to an external probe;
// there is nothing to diff against. In-process, BOTH trees are reachable on the
// UI thread: the real WPF visual/logical tree (graph 1, "what a pointer user can
// operate") AND the UIAutomation peer tree (graph 2, "what a11y + keyboard can
// reach"). The peer is created FROM the element, so the graph-1<->graph-2 join
// is by OBJECT IDENTITY, not geometry or name. That join is the whole point.
//
// We build a tiny tree with two siblings:
//   (a) a real <Button> "save"           -> accessible, clean
//   (b) a "fake button": a <Border>/<TextBlock> "delete" with a
//       MouseLeftButtonUp handler and NO Button role / NO AutomationProperties
//       (the WPF analogue of a clickable <div>) -> operable by mouse, role-less
//       and not keyboard-activatable to UIA -> a WCAG 2.1.1 / 4.1.2 gap.
//
// We then emit ONE line:
//   EXPLORE:GROUNDTRUTH {"sig":...,"focusTrap":false,"elements":[...]}
// parsed by crates/reproit/src/model/map.rs::gaps_from_groundtruth. The engine
// rule: an operable element is a gap if a11y.keyboardActivatable==false OR
// inTabOrder==false OR rolePresent==false (missing dims default to true).

using System;
using System.Collections;
using System.Collections.Generic;
using System.Reflection;
using System.Threading;
using System.Windows;
using System.Windows.Automation.Peers;
using System.Windows.Automation.Provider;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;
using ReproIt.Core;

namespace ReproIt.WpfAgent
{
    public static class Program
    {
        // STAThread is mandatory: WPF UI objects (Dispatcher, AutomationPeers,
        // routed events) only function on a single-threaded-apartment thread.
        [STAThread]
        public static int Main(string[] args)
        {
            int exitCode = 0;
            // We deliberately drive our own Dispatcher rather than instantiating
            // System.Windows.Application: this lets the agent run as a console
            // emitter over a non-interactive SSH session (no App message loop /
            // no resource dictionaries needed). The visual tree, AutomationPeers
            // and routed-event plumbing all work on a bare Dispatcher thread.
            var done = new ManualResetEventSlim(false);
            var dispatcher = Dispatcher.CurrentDispatcher;

            dispatcher.BeginInvoke(
                new Action(() =>
                           {
                               try
                               {
                                   exitCode = Run(realWindow: !HasNoWindow(args));
                               }
                               catch (Exception ex)
                               {
                                   Console.Error.WriteLine("agent error: " + ex);
                                   exitCode = 2;
                               }
                               finally
                               {
                                   done.Set();
                                   dispatcher.BeginInvokeShutdown(DispatcherPriority.Background);
                               }
                           }));

            Dispatcher.Run();
            done.Wait();
            return exitCode;
        }

        private static bool HasNoWindow(string[] args)
        {
            foreach (var a in args)
            {
                if (string.Equals(a, "--no-window", StringComparison.OrdinalIgnoreCase))
                {
                    return true;
                }
            }
            return false;
        }

        // ------------------------------------------------------------------ //
        // The agent: build the tree, walk both graphs, join, emit the marker. //
        // ------------------------------------------------------------------ //

        private static int Run(bool realWindow)
        {
            // ---- build the screen --------------------------------------------
            var panel = new StackPanel();

            // (a) the REAL button: a first-class control. ButtonBase => operable
            // ground truth; it also produces a ButtonAutomationPeer with a Button
            // control type, a name, keyboard focusability and an InvokePattern.
            var realButton = new Button {
                Name = "SaveButton",
                Content = "Save",
            };
            realButton.Click += (s, e) =>
            { /* real handler */ };
            panel.Children.Add(realButton);

            // (b) the FAKE button: a Border wrapping a TextBlock with a mouse
            // handler and NO Button role / NO AutomationProperties. Operable by
            // pointer (it has a MouseLeftButtonUp handler + IsHitTestVisible +
            // IsEnabled), but to UIA it is a plain element: no Button control
            // type, not keyboard-focusable, no InvokePattern. The div-soup gap.
            var fakeText = new TextBlock { Text = "Delete" };
            var fakeButton = new Border {
                Name = "DeleteFakeButton",
                Background = Brushes.Transparent,
                Child = fakeText,
                Focusable = false,
            };
            fakeButton.MouseLeftButtonUp += (s, e) =>
            { /* fake click handler */ };
            panel.Children.Add(fakeButton);

            // (c) a COLLAPSED fake button: operable by pointer (mouse handler) but
            // Visibility=Collapsed, so it is reachable by neither pointer nor
            // keyboard. The reachability gate must prune it (and its subtree) so it
            // is NOT reported as a gap (it has no role either, so it should not
            // appear in the marker at all).
            var hiddenText = new TextBlock { Text = "Hidden Delete" };
            var hiddenFake = new Border {
                Name = "HiddenFakeButton", Background = Brushes.Transparent,  Child = hiddenText,
                Focusable = false,         Visibility = Visibility.Collapsed,
            };
            hiddenFake.MouseLeftButtonUp += (s, e) =>
            { /* unreachable */ };
            panel.Children.Add(hiddenFake);

            // Make the tree LIVE so AutomationPeers resolve. A real, shown Window
            // is the faithful path (needs a desktop session); when none is
            // available we fall back to an off-screen measured/arranged window,
            // which still connects the elements to a PresentationSource-less
            // visual tree well enough for VisualTreeHelper + the peers we read.
            var window = new Window {
                Title = "ReproIt WPF Operability Agent",
                Width = 320,
                Height = 200,
                Content = panel,
                ShowInTaskbar = false,
            };

            bool shown = false;
            if (realWindow)
            {
                try
                {
                    window.WindowStyle = WindowStyle.ToolWindow;
                    window.ShowActivated = false;
                    window.Left = -10000; // off the visible desktop, still real
                    window.Top = -10000;
                    window.Show();
                    shown = true;
                }
                catch (Exception ex)
                {
                    Console.Error.WriteLine("note: Window.Show() failed (" + ex.GetType().Name +
                                            ": " + ex.Message +
                                            "), falling back to off-screen Measure/Arrange.");
                }
            }
            if (!shown)
            {
                // No interactive desktop: still drive layout so the tree is
                // measured/arranged and the peers are creatable.
                window.Measure(new Size(320, 200));
                window.Arrange(new Rect(0, 0, 320, 200));
                window.UpdateLayout();
            }

            // ---- walk graph 1 + graph 2, joined by object identity -----------
            var elements = new List<object>();
            int visualIndex = 0;
            WalkAndJoin(window, ref visualIndex, elements);

            // The shared structural signature, computed via the canonical
            // ReproIt.Core oracle (FNV-1a 32-bit) so `sig` is real, not a stub.
            string sig = ComputeSignature(window);

            var record = new OrderedDictionary {
                { "sig", sig },
                { "focusTrap", false },
                { "elements", elements },
            };
            Console.WriteLine("EXPLORE:GROUNDTRUTH " + Json.Encode(record));

            if (shown)
            {
                window.Close();
            }
            return 0;
        }

        // Walk the visual tree in pre-order. For each element compute the
        // ground-truth operability (graph 1) and, by creating its AutomationPeer
        // (graph 2), the a11y dimensions. Only elements that are operable OR
        // emit an a11y signal are reported (chrome panels are skipped to keep
        // the marker focused, exactly like a real backend's node filter).
        private static void WalkAndJoin(DependencyObject node, ref int visualIndex,
                                        List<object> outList)
        {
            // Reachability: prune hidden / collapsed subtrees. An element a user
            // can reach with neither pointer nor keyboard is operable by nobody,
            // so it must not be scored as a gap (mirrors the web runner's
            // reachability gate on `operable`). We test Visibility, which is
            // layout-free and works in the headless Measure/Arrange path, rather
            // than IsVisible, which needs a rendered PresentationSource the
            // in-process agent may not have.
            if (node is UIElement vis && vis.Visibility != Visibility.Visible)
            {
                return; // skip this element AND its descendants
            }
            if (node is UIElement el)
            {
                var g1 = GroundTruth(el);
                if (g1.Operable)
                {
                    var a11y = Accessibility(el);
                    string id = StableId(el, visualIndex);
                    var elementRecord = new OrderedDictionary {
                        { "id", id },
                        { "operable", true },
                        { "gestureKind", g1.GestureKind },
                        { "a11y",
                          new OrderedDictionary {
                              { "rolePresent", a11y.RolePresent },
                              { "namePresent", a11y.NamePresent },
                              { "focusable", a11y.Focusable },
                              { "inTabOrder", a11y.InTabOrder },
                              { "keyboardActivatable", a11y.KeyboardActivatable },
                          } },
                    };
                    outList.Add(elementRecord);
                    visualIndex++;
                }
            }

            int count = VisualTreeHelper.GetChildrenCount(node);
            for (int i = 0; i < count; i++)
            {
                WalkAndJoin(VisualTreeHelper.GetChild(node, i), ref visualIndex, outList);
            }
        }

        // ---- graph 1: ground-truth pointer/gesture operability ---------------
        private struct G1
        {
            public bool Operable;
            public string GestureKind;
        }

        private static G1 GroundTruth(UIElement el)
        {
            // Not operable if it cannot be hit-tested or is disabled.
            if (!el.IsHitTestVisible || !el.IsEnabled)
            {
                return new G1 { Operable = false, GestureKind = null };
            }

            // First-class interactive controls (the WPF "real control" surface).
            if (el is System.Windows.Controls.Primitives.ButtonBase)
            {
                return new G1 { Operable = true, GestureKind = "button" };
            }
            // (Hyperlink is a ContentElement, not a UIElement, so it never
            // appears in this UIElement-typed visual walk; link affordances are
            // covered by the pointer-handler path below when relevant.)
            if (el is TextBox || el is PasswordBox)
            {
                return new G1 { Operable = true, GestureKind = "field" };
            }

            // The div-soup case: any element carrying a pointer/click routed-event
            // handler is operable by pointer even with no control role. WPF stores
            // instance handlers in the EventHandlersStore (private), reachable via
            // reflection on UIElement. We detect handlers for the mouse-button /
            // touch / preview routed events and the high-level MouseDown family.
            if (HasPointerHandler(el))
            {
                return new G1 { Operable = true, GestureKind = "delegated" };
            }

            return new G1 { Operable = false, GestureKind = null };
        }

        // The pointer/click routed events whose presence on an element means a
        // sighted pointer user can operate it (the graph-1 affordance signal).
        private static readonly RoutedEvent[] PointerEvents = new[] {
            UIElement.MouseLeftButtonUpEvent,
            UIElement.MouseLeftButtonDownEvent,
            UIElement.MouseDownEvent,
            UIElement.MouseUpEvent,
            UIElement.PreviewMouseLeftButtonUpEvent,
            UIElement.PreviewMouseLeftButtonDownEvent,
            UIElement.PreviewMouseUpEvent,
            UIElement.PreviewMouseDownEvent,
            UIElement.TouchUpEvent,
            UIElement.TouchDownEvent,
        };

        // Reflect into UIElement's private EventHandlersStore to see whether the
        // element has any instance handler for a pointer routed event. This is
        // how the agent finds a clickable Border/TextBlock that exposes no role.
        private static bool HasPointerHandler(UIElement el)
        {
            try
            {
                var storeProp = typeof(UIElement).GetProperty(
                    "EventHandlersStore", BindingFlags.Instance | BindingFlags.NonPublic);
                var store = storeProp?.GetValue(el);
                if (store == null)
                {
                    return false;
                }
                var getMethod = store.GetType().GetMethod(
                    "GetRoutedEventHandlers",
                    BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);
                if (getMethod == null)
                {
                    return false;
                }
                foreach (var re in PointerEvents)
                {
                    if (re == null)
                    {
                        continue;
                    }
                    var handlers =
                        getMethod.Invoke(store, new object[] { re }) as RoutedEventHandlerInfo[];
                    if (handlers != null && handlers.Length > 0)
                    {
                        return true;
                    }
                }
            }
            catch
            {
                // Reflection shape changed: fail closed (no handler detected).
            }
            return false;
        }

        // ---- graph 2: accessibility / keyboard reachability ------------------
        private struct A11y
        {
            public bool RolePresent;
            public bool NamePresent;
            public bool Focusable;
            public bool InTabOrder;
            public bool KeyboardActivatable;
        }

        private static A11y Accessibility(UIElement el)
        {
            var a = new A11y();

            // The peer is created FROM the element: this IS the object-identity
            // join between graph 1 (the element) and graph 2 (its a11y view).
            AutomationPeer peer = UIElementAutomationPeer.CreatePeerForElement(el);

            if (peer != null)
            {
                var ct = peer.GetAutomationControlType();
                // A real role means UIA reports a specific control type. WPF gives
                // a role-less element the generic "Custom" type (or Pane/Group for
                // bare containers); those do not constitute an operable role for a
                // control, so we treat Custom as "no role present".
                a.RolePresent = ct != AutomationControlType.Custom;

                string name = null;
                try
                {
                    name = peer.GetName();
                }
                catch
                { /* some peers throw pre-render */
                }
                a.NamePresent = !string.IsNullOrEmpty(name);

                a.Focusable = peer.IsKeyboardFocusable();

                // keyboardActivatable: does it expose an Invoke (or Toggle/
                // SelectionItem) pattern a keyboard user could fire? A clickable
                // Border has none.
                bool hasInvoke = peer.GetPattern(PatternInterface.Invoke) is IInvokeProvider;
                bool hasToggle = peer.GetPattern(PatternInterface.Toggle) != null;
                bool hasSelItem = peer.GetPattern(PatternInterface.SelectionItem) != null;
                a.KeyboardActivatable = a.Focusable && (hasInvoke || hasToggle || hasSelItem);
            }
            else
            {
                a.RolePresent = false;
                a.NamePresent = false;
                a.Focusable = false;
                a.KeyboardActivatable = false;
            }

            // inTabOrder: reachable by Tab. KeyboardNavigation.IsTabStop must be
            // true AND the element must be focusable; a non-focusable Border with
            // IsTabStop unset is not in the tab sequence.
            bool isTabStop = KeyboardNavigation.GetIsTabStop(el);
            a.InTabOrder = isTabStop && a.Focusable;

            return a;
        }

        // The join/selector id: AutomationId if set, else x:Name, else a stable
        // visual-index fallback (matches reproit's selector grammar intent).
        private static string StableId(UIElement el, int visualIndex)
        {
            if (el is FrameworkElement fe)
            {
                string autoId = System.Windows.Automation.AutomationProperties.GetAutomationId(fe);
                if (!string.IsNullOrEmpty(autoId))
                {
                    return autoId;
                }
                if (!string.IsNullOrEmpty(fe.Name))
                {
                    return fe.Name;
                }
            }
            return "visual#" +
                   visualIndex.ToString(System.Globalization.CultureInfo.InvariantCulture);
        }

        // Build a ReproIt.Core.Node tree from the visual tree and hash it with the
        // canonical signature oracle, so `sig` matches every other reproit SDK.
        private static string ComputeSignature(Window window)
        {
            var root = new Node("screen");
            BuildSigChildren(window, root);
            return Signature.Of(null, root);
        }

        private static void BuildSigChildren(DependencyObject node, Node parent)
        {
            int count = VisualTreeHelper.GetChildrenCount(node);
            for (int i = 0; i < count; i++)
            {
                var child = VisualTreeHelper.GetChild(node, i);
                Node sigNode = null;
                if (child is UIElement el)
                {
                    var g1 = GroundTruth(el);
                    string role = g1.Operable ? (g1.GestureKind == "field"  ? "textfield"
                                                 : g1.GestureKind == "link" ? "link"
                                                                            : "button")
                                              : null;
                    if (role != null)
                    {
                        sigNode = new Node(role) { Id = StableId(el, 0) };
                        parent.Children.Add(sigNode);
                    }
                }
                BuildSigChildren(child, sigNode ?? parent);
            }
        }
    }

    // A tiny insertion-ordered string->object map so Json.Encode emits fields in
    // the exact order the marker spec lists them. ReproIt.Core.Json serializes a
    // value as a JSON OBJECT only when it is an IDictionary<string, object> (the
    // non-generic IDictionary path does not exist there and would be treated as a
    // sequence), so this MUST implement the generic interface.
    internal sealed class OrderedDictionary : IDictionary<string, object>
    {
        private readonly List<string> _keys = new List<string>();
        private readonly Dictionary<string, object> _map =
            new Dictionary<string, object>(StringComparer.Ordinal);

        public void Add(string key, object value)
        {
            _keys.Add(key);
            _map.Add(key, value);
        }

        public object this[string key]
        {
            get
            {
                return _map[key];
            }
            set
            {
                if (!_map.ContainsKey(key))
                {
                    _keys.Add(key);
                }
                _map[key] = value;
            }
        }

        public ICollection<string> Keys => _keys;
        public ICollection<object> Values
        {
            get {
                var v = new List<object>();
                foreach (var k in _keys)
                {
                    v.Add(_map[k]);
                }
                return v;
            }
        }

        public bool ContainsKey(string key) => _map.ContainsKey(key);
        public bool TryGetValue(string key, out object value) => _map.TryGetValue(key, out value);
        public void Add(KeyValuePair<string, object> item) => Add(item.Key, item.Value);
        public bool Contains(KeyValuePair<string, object> item) => _map.ContainsKey(item.Key);
        public bool Remove(string key)
        {
            _keys.Remove(key);
            return _map.Remove(key);
        }
        public bool Remove(KeyValuePair<string, object> item) => Remove(item.Key);
        public void Clear()
        {
            _keys.Clear();
            _map.Clear();
        }
        public void CopyTo(KeyValuePair<string, object>[] array, int index)
        {
            foreach (var k in _keys)
            {
                array[index++] = new KeyValuePair<string, object>(k, _map[k]);
            }
        }
        public int Count => _keys.Count;
        public bool IsReadOnly => false;

        public IEnumerator<KeyValuePair<string, object>> GetEnumerator()
        {
            foreach (var k in _keys)
            {
                yield return new KeyValuePair<string, object>(k, _map[k]);
            }
        }

        IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
    }
}
