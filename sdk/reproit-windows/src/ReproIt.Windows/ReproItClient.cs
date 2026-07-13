// ReproIt production telemetry for native Windows (.NET): WPF (System.Windows)
// and WinUI 3 (Microsoft.UI.Xaml).
//
// Emits the SAME state-graph and error events from real users that the reproit
// test runners emit, so the production graph aligns 1:1 with test-time graphs and
// a prod "cannot reproduce" becomes a deterministic replay. The signature and
// payload shapes are byte-identical to sdk/reproit-web.js, the Android / iOS /
// Flutter / React-Native SDKs, and the runners (windows-uia.py, ...).
//
// The heavy, deterministic logic (signature, snapshot reduction, batching, JSON)
// lives in the cross-platform ReproIt.Core library; this class is the thin
// Windows binding (root discovery, the debounced visual-tree walk, crash
// handlers, HTTP). The whole file is reflection-driven over the XAML surface so
// the one assembly serves both WPF and WinUI 3 hosts.
//
// Usage (WPF), in App.OnStartup or the main window constructor:
//
//   ReproItClient.Init(new ReproItConfig("example")
//   {
//       Endpoint = "https://ingest.reproit.example",
//       ApiKey = "sk_...",
//   });
//   ReproItClient.Attach(this); // `this` is the Window
//
// Usage (WinUI 3), after the main Window is created:
//
//   ReproItClient.Init(new ReproItConfig("example") { Endpoint = "...", ApiKey = "..." });
//   ReproItClient.Attach(mainWindow);

using System;
using System.Collections.Generic;
using System.Globalization;
using System.Net.Http;
using System.Reflection;
using System.Text;
using System.Threading;
using ReproIt.Core;

namespace ReproIt.Windows
{
    public static class ReproItClient
    {
        private static Engine _engine;
        private static ReproItConfig _cfg;
        private static object _root;            // the attached Window / root element
        private static string _anchor;          // developer-supplied screen anchor
        private static bool _firstObserved;
        private static Timer _debounceTimer;
        private static Timer _flushTimer;
        private static readonly object _gate = new object();
        private static readonly HttpClient Http = new HttpClient();

        /// <summary>Initialize telemetry. Safe to call once; later calls are ignored.
        /// Installs the crash handlers and starts the flush timer. Call
        /// <see cref="Attach"/> with your main Window to begin capturing.</summary>
        public static void Init(ReproItConfig config)
        {
            lock (_gate)
            {
                if (_engine != null)
                {
                    return;
                }
                // Sampling decision, made once per session.
                if (config.SampleRate < 1.0 && new Random().NextDouble() > config.SampleRate)
                {
                    return;
                }
                _cfg = config;
                _engine = new Engine(
                    cfg: config,
                    now: () => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
                    transport: body => Post(config, body),
                    log: msg => System.Diagnostics.Debug.WriteLine("reproit " + msg));

                // Tier-1 auto dimensions: zero-PII, high-signal for "works for me but
                // not for them" bugs. Mirrors the other SDKs' init-time dimensions.
                _engine.SetContexts(new Dictionary<string, object>
                {
                    { "platform", "winui" },
                    { "os", Environment.OSVersion.Version.ToString() },
                    { "locale", CultureInfo.CurrentUICulture.Name },
                    { "tz", TimeZoneInfo.Local.Id },
                });

                InstallCrashHandlers();
                _flushTimer = new Timer(_ => Flush(), null, config.FlushMs, config.FlushMs);
            }
        }

        /// <summary>Zero-config start: the one-line quickstart. Begins telemetry
        /// with sensible defaults and no required configuration, then attaches to
        /// <paramref name="window"/> when one is given (pass your main Window).
        /// Enabled only for a Debug build (the entry assembly's DebuggableAttribute,
        /// which Release builds omit); a no-op otherwise, so shipping this one line
        /// does nothing in a Release build. The app id is derived from the entry
        /// assembly name. To run in a Release build, or to override any field, call
        /// <see cref="Init"/> with an explicit ReproItConfig.</summary>
        public static void Start(object window = null)
        {
            if (!IsDebugBuild())
            {
                return;
            }
            string appId = Assembly.GetEntryAssembly()?.GetName()?.Name ?? "app";
            Init(new ReproItConfig(appId));
            if (window != null)
            {
                Attach(window);
            }
        }

        /// <summary>Whether the entry assembly was built in Debug configuration
        /// (its JIT optimizer is disabled). Release builds return false, so
        /// <see cref="Start"/> no-ops there.</summary>
        private static bool IsDebugBuild()
        {
            try
            {
                Assembly asm = Assembly.GetEntryAssembly() ?? Assembly.GetCallingAssembly();
                var attr = (System.Diagnostics.DebuggableAttribute)Attribute.GetCustomAttribute(
                    asm, typeof(System.Diagnostics.DebuggableAttribute));
                return attr != null && attr.IsJITOptimizerDisabled;
            }
            catch
            {
                return false;
            }
        }

        /// <summary>Attach to a Window (WPF System.Windows.Window or WinUI
        /// Microsoft.UI.Xaml.Window) or any XAML root element. Wires layout-change
        /// observation for debounced snapshots and pointer observation for taps.</summary>
        public static void Attach(object window)
        {
            if (_engine == null || window == null)
            {
                return;
            }
            _root = window;
            WireLayoutObserver(window);
            WirePointerObserver(window);
            ScheduleSnapshot();
        }

        /// <summary>Set the current screen anchor (route / screen name). This becomes
        /// the "A:" prefix of the structural signature, so two same-shaped screens at
        /// different routes hash distinctly, and a wizard's steps at one route split.
        /// Pass null to clear.</summary>
        public static void Screen(string name)
        {
            _anchor = string.IsNullOrEmpty(name) ? null : name;
        }

        // ---- developer annotation helpers (attached marks) ----------------------

        /// <summary>Tag an element with a stable structural id (overrides AutomationId / x:Name).</summary>
        public static void TagId(object element, string id)
        {
            Capture.MarksFor(element, true).Id = id;
        }

        /// <summary>Tag an element with a language-independent icon identity.</summary>
        public static void TagIcon(object element, string icon)
        {
            Capture.MarksFor(element, true).Icon = icon;
        }

        /// <summary>Mark an element (and its subtree) transient so it is dropped from the hash.</summary>
        public static void TagTransient(object element)
        {
            Capture.MarksFor(element, true).Transient = true;
        }

        /// <summary>Mark an element as value-bearing (Layer 3 opt-in). Its displayed
        /// value (its Text, or the supplied <paramref name="value"/>) folds into the
        /// canonical signature as a bounded, locale-safe value-class even when the
        /// element's role is not a structural value-role. Use for counters / scores /
        /// stopwatches shown in plain TextBlocks where structure never moves.</summary>
        public static void TagValue(object element, string value = null)
        {
            var m = Capture.MarksFor(element, true);
            m.ValueNode = true;
            m.Value = value;
        }

        /// <summary>Flush queued events immediately (e.g. before a known teardown).</summary>
        public static void Flush()
        {
            _engine?.Flush();
        }

        /// <summary>Create an HttpClient that automatically participates in a
        /// Reproit causal run and behaves like a normal client otherwise.</summary>
        public static HttpClient CreateHttpClient(HttpMessageHandler innerHandler = null)
        {
            return new HttpClient(new ReproItCausalHandler(innerHandler));
        }

        /// <summary>Attach a hashed user id plus optional PII-safe context dimensions.
        /// The raw id is never stored or sent; only a SHA-256 hex prefix ("uid").</summary>
        public static void Identify(string userId, IDictionary<string, object> context = null)
        {
            _engine?.Identify(userId, context);
        }

        /// <summary>Set a single PII-safe context dimension (e.g. role, plan, a bucket).</summary>
        public static void SetContext(string key, object value)
        {
            _engine?.SetContext(key, value);
        }

        /// <summary>Merge several PII-safe context dimensions at once.</summary>
        public static void SetContexts(IDictionary<string, object> values)
        {
            _engine?.SetContexts(values);
        }

        // ---- snapshot scheduling + capture --------------------------------------

        private static void ScheduleSnapshot()
        {
            var cfg = _cfg;
            if (cfg == null)
            {
                return;
            }
            // Debounce: take the snapshot once the UI has been quiet for DebounceMs.
            lock (_gate)
            {
                _debounceTimer?.Dispose();
                _debounceTimer = new Timer(_ => RunOnUi(TakeSnapshot), null, cfg.DebounceMs, Timeout.Infinite);
            }
        }

        private static void TakeSnapshot()
        {
            var e = _engine;
            object content = RootContent();
            if (e == null || content == null)
            {
                return;
            }
            try
            {
                var nodes = new List<RawNode>();
                Capture.WalkLabels(content, nodes);
                if (nodes.Count == 0 && _firstObserved)
                {
                    return;
                }
                Node tree = Capture.CaptureTree(content);
                var snap = e.Reduce(nodes, tree, _anchor);
                e.Observe(snap, firstAction: "load");
                _firstObserved = true;
            }
            catch
            {
                // a capture failure must never crash the host app.
            }
        }

        /// <summary>The content root of the attached Window (its Content), or the
        /// attached element itself if it is not a Window.</summary>
        private static object RootContent()
        {
            object root = _root;
            if (root == null)
            {
                return null;
            }
            // A Window exposes its visual root through Content (both WPF and WinUI).
            object content = GetProperty(root, "Content");
            return content ?? root;
        }

        // ---- layout + pointer observation (reflection-wired events) -------------

        private static void WireLayoutObserver(object window)
        {
            // Both stacks raise a LayoutUpdated event on FrameworkElement and a
            // SizeChanged event on Window; subscribing reflectively keeps this
            // framework-agnostic. A snapshot is (re)scheduled on each, debounced.
            HookEvent(RootContent() ?? window, "LayoutUpdated", (s, a) => ScheduleSnapshot());
            HookEvent(window, "SizeChanged", (s, a) => ScheduleSnapshot());
            HookEvent(window, "Activated", (s, a) => ScheduleSnapshot());
        }

        private static void WirePointerObserver(object window)
        {
            // WPF: PreviewMouseDown (RoutedEventHandler-ish). WinUI: PointerPressed.
            // We subscribe to whichever exists; the handler hit-tests for a tap label.
            // Both are pass-through (we never set Handled), so the app's own input is
            // unaffected.
            HookEvent(window, "PreviewMouseDown", OnPointer);
            HookEvent(window, "PointerPressed", OnPointer);
        }

        private static void OnPointer(object sender, object args)
        {
            try
            {
                // Best-effort target: the event source's structural selector plus
                // accessible name. Full hit-testing differs per stack; unresolved
                // sources become tap:? rather than a non-replayable label action.
                object source = GetProperty(args, "OriginalSource") ?? GetProperty(args, "Source") ?? sender;
                string label = source != null ? Capture.NameOf(source) : null;
                string clean = _engine?.CleanLabel(label);
                string selector = source != null ? Capture.SelectorOf(source) : null;
                _engine?.NoteTap(selector, clean);
            }
            catch
            {
                _engine?.NoteTap(null, null);
            }
        }

        /// <summary>Subscribe a (object,object) handler to an event by name via
        /// reflection, adapting it to whatever delegate the event declares. Silently
        /// no-ops if the event does not exist on this framework/element.</summary>
        private static void HookEvent(object target, string eventName, Action<object, object> handler)
        {
            if (target == null)
            {
                return;
            }
            try
            {
                EventInfo ev = target.GetType().GetEvent(eventName, BindingFlags.Public | BindingFlags.Instance);
                if (ev == null)
                {
                    return;
                }
                Type handlerType = ev.EventHandlerType;
                MethodInfo invoke = handlerType.GetMethod("Invoke");
                ParameterInfo[] ps = invoke.GetParameters();
                // Build a delegate of the event's exact type that forwards (sender, args).
                Delegate del = BuildForwarder(handlerType, ps.Length, handler);
                ev.AddEventHandler(target, del);
            }
            catch
            {
                // an event we cannot bind is simply not observed.
            }
        }

        private static Delegate BuildForwarder(Type handlerType, int paramCount, Action<object, object> handler)
        {
            // Most XAML events are (object sender, TArgs e). We create a matching
            // delegate through a small closure forwarder. For the common 2-arg shape
            // we forward (sender, args); for any other arity we forward nulls for the
            // missing slots. Implemented with a dynamic method-free approach using a
            // generic helper instance whose Handle method matches the signature shape.
            var fwd = new Forwarder(handler);
            // The event handler signature is (object, T). Bind Forwarder.Handle, which
            // is (object, object); the runtime will adapt T (a reference type) to
            // object for delegate creation when T is a reference type. XAML event arg
            // types are reference types, so this binds for the standard shape.
            MethodInfo handle = typeof(Forwarder).GetMethod(nameof(Forwarder.Handle), BindingFlags.Public | BindingFlags.Instance);
            try
            {
                return Delegate.CreateDelegate(handlerType, fwd, handle);
            }
            catch
            {
                // If the exact arg type is not object-assignable for delegate binding,
                // fall back to a parameterless adapter where the event allows it.
                MethodInfo handle0 = typeof(Forwarder).GetMethod(nameof(Forwarder.Handle0), BindingFlags.Public | BindingFlags.Instance);
                return Delegate.CreateDelegate(handlerType, fwd, handle0);
            }
        }

        private sealed class Forwarder
        {
            private readonly Action<object, object> _h;

            public Forwarder(Action<object, object> h)
            {
                _h = h;
            }

            public void Handle(object sender, object args)
            {
                _h(sender, args);
            }

            public void Handle0()
            {
                _h(null, null);
            }
        }

        /// <summary>Run an action on the UI thread when a XAML Dispatcher is reachable;
        /// otherwise run it inline. Capture must touch XAML objects on the UI thread.</summary>
        private static void RunOnUi(Action action)
        {
            object root = _root;
            object dispatcher = GetProperty(root, "Dispatcher") ?? GetProperty(RootContent(), "Dispatcher");
            if (dispatcher == null)
            {
                action();
                return;
            }
            try
            {
                // WPF: Dispatcher.BeginInvoke(Action). WinUI: DispatcherQueue.TryEnqueue.
                MethodInfo begin = dispatcher.GetType().GetMethod("BeginInvoke", new[] { typeof(Delegate), typeof(object[]) })
                                   ?? dispatcher.GetType().GetMethod("BeginInvoke", new[] { typeof(Action) });
                if (begin != null)
                {
                    if (begin.GetParameters().Length == 2)
                    {
                        begin.Invoke(dispatcher, new object[] { action, new object[0] });
                    }
                    else
                    {
                        begin.Invoke(dispatcher, new object[] { action });
                    }
                    return;
                }
                MethodInfo tryEnqueue = dispatcher.GetType().GetMethod("TryEnqueue", new[] { typeof(Action) });
                if (tryEnqueue != null)
                {
                    tryEnqueue.Invoke(dispatcher, new object[] { action });
                    return;
                }
            }
            catch
            {
                // fall through to inline
            }
            action();
        }

        // ---- crash handlers ------------------------------------------------------

        private static void InstallCrashHandlers()
        {
            // 1. AppDomain.UnhandledException: the last-chance handler for any thread.
            AppDomain.CurrentDomain.UnhandledException += (sender, e) =>
            {
                var ex = e.ExceptionObject as Exception;
                RecordCrash(ex, "AppDomain.UnhandledException");
                _engine?.Flush(); // synchronous best-effort flush before the process dies
            };

            // 2. The XAML DispatcherUnhandledException (WPF Application.DispatcherUnhandledException
            //    or WinUI Application.UnhandledException), wired reflectively against the
            //    current Application instance so we do not hard-link either framework.
            try
            {
                object app = CurrentApplication();
                if (app != null)
                {
                    HookEvent(app, "DispatcherUnhandledException", OnDispatcherUnhandled); // WPF
                    HookEvent(app, "UnhandledException", OnDispatcherUnhandled);           // WinUI
                }
            }
            catch
            {
                // no Application yet / not a XAML app: the AppDomain handler still applies.
            }

            // 3. Unobserved task exceptions, so async faults are captured too.
            System.Threading.Tasks.TaskScheduler.UnobservedTaskException += (sender, e) =>
            {
                RecordCrash(e.Exception, "UnobservedTaskException");
            };
        }

        private static void OnDispatcherUnhandled(object sender, object args)
        {
            // The args carry an Exception property on both stacks.
            var ex = GetProperty(args, "Exception") as Exception;
            RecordCrash(ex, "DispatcherUnhandledException");
            _engine?.Flush();
        }

        private static void RecordCrash(Exception ex, string sourceTag)
        {
            try
            {
                if (ex == null)
                {
                    _engine?.RecordError(sourceTag, new List<string>(), source: sourceTag);
                    return;
                }
                var stack = SplitStack(ex);
                string top = stack.Count > 0 ? stack[0] : string.Empty;
                _engine?.RecordError(
                    message: ex.GetType().Name + ": " + ex.Message,
                    stack: stack,
                    source: top,
                    line: 0,
                    context: null);
            }
            catch
            {
                // never throw from the crash handler.
            }
        }

        private static List<string> SplitStack(Exception ex)
        {
            var lines = new List<string>();
            string trace = ex.StackTrace ?? string.Empty;
            foreach (var line in trace.Split('\n'))
            {
                string t = line.Trim();
                if (t.Length > 0)
                {
                    lines.Add(t);
                }
                if (lines.Count >= 8)
                {
                    break;
                }
            }
            return lines;
        }

        private static object CurrentApplication()
        {
            // WPF: System.Windows.Application.Current. WinUI: Microsoft.UI.Xaml.Application.Current.
            string[] candidates =
            {
                "System.Windows.Application, PresentationFramework",
                "Microsoft.UI.Xaml.Application, Microsoft.WinUI",
            };
            foreach (var name in candidates)
            {
                try
                {
                    Type t = Type.GetType(name, false);
                    PropertyInfo cur = t?.GetProperty("Current", BindingFlags.Public | BindingFlags.Static);
                    object app = cur?.GetValue(null);
                    if (app != null)
                    {
                        return app;
                    }
                }
                catch
                {
                    // try the next candidate
                }
            }
            // Fallback scan of loaded assemblies.
            foreach (var asm in AppDomain.CurrentDomain.GetAssemblies())
            {
                try
                {
                    Type t = asm.GetType("System.Windows.Application", false)
                             ?? asm.GetType("Microsoft.UI.Xaml.Application", false);
                    object app = t?.GetProperty("Current", BindingFlags.Public | BindingFlags.Static)?.GetValue(null);
                    if (app != null)
                    {
                        return app;
                    }
                }
                catch
                {
                    // ignore
                }
            }
            return null;
        }

        // ---- transport -----------------------------------------------------------

        private static bool Post(ReproItConfig config, string body)
        {
            if (config.Endpoint == null)
            {
                return true;
            }
            try
            {
                var req = new HttpRequestMessage(HttpMethod.Post, config.Endpoint.TrimEnd('/') + "/v1/events");
                req.Content = new StringContent(body, Encoding.UTF8, "application/json");
                if (!string.IsNullOrEmpty(config.ApiKey))
                {
                    req.Headers.TryAddWithoutValidation("Authorization", "Bearer " + config.ApiKey);
                }
                using (var resp = Http.Send(req))
                {
                    return (int)resp.StatusCode >= 200 && (int)resp.StatusCode < 300;
                }
            }
            catch
            {
                return false;
            }
        }

        // ---- reflection helper ---------------------------------------------------

        private static object GetProperty(object o, string name)
        {
            if (o == null)
            {
                return null;
            }
            try
            {
                PropertyInfo pi = o.GetType().GetProperty(name, BindingFlags.Public | BindingFlags.Instance);
                return pi != null && pi.CanRead ? pi.GetValue(o) : null;
            }
            catch
            {
                return null;
            }
        }
    }
}
