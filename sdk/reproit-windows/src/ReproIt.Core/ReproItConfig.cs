// Configuration for ReproItClient (the Windows binding) and the host-testable
// Engine. Field names and defaults mirror the web SDK (sdk/reproit-web.js) and
// the Android / Flutter / iOS SDKs so behavior is consistent across platforms.

using System;
using System.Collections.Generic;

namespace ReproIt.Core
{
    public sealed class ReproItConfig
    {
        /// <summary>Identifies the app in the cloud (the "appId" in every batch). Required.</summary>
        public string AppId { get; set; }

        /// <summary>POST &lt;endpoint&gt;/v1/events. If null, events go only to OnEvent/log.</summary>
        public string Endpoint { get; set; }

        /// <summary>Bearer token sent as "Authorization: Bearer &lt;apiKey&gt;" when set.</summary>
        public string ApiKey { get; set; }

        /// <summary>User-visible application version stamped into ctx.build.version.</summary>
        public string BuildVersion { get; set; }

        /// <summary>Source revision stamped into ctx.build.commit.</summary>
        public string BuildCommit { get; set; }

        /// <summary>Dev hook / custom transport; called for every event in addition to
        /// (or instead of, when Endpoint is null) the HTTP sink. The dictionary is the
        /// event exactly as it will be serialized.</summary>
        public Action<IDictionary<string, object>> OnEvent { get; set; }

        /// <summary>Fraction of sessions that report (0..1). Decided once at init.</summary>
        public double SampleRate { get; set; } = 1.0;

        /// <summary>Max distinct labels captured per state (matches the runners).</summary>
        public int MaxLabels { get; set; } = 24;

        /// <summary>Labels longer than this are ignored (matches the runners).</summary>
        public int MaxLabelLen { get; set; } = 40;

        /// <summary>Max length of the action trail kept for repro paths.</summary>
        public int PathCap { get; set; } = 60;

        /// <summary>How often batched events are flushed, in milliseconds.</summary>
        public long FlushMs { get; set; } = 5000;

        /// <summary>When true, only signatures are sent (no human-readable labels).</summary>
        public bool RedactLabels { get; set; }

        /// <summary>Settle window: snapshot once the UI has been quiet this long, in ms.</summary>
        public long DebounceMs { get; set; } = 350;

        public ReproItConfig(string appId)
        {
            AppId = appId;
        }
    }
}
