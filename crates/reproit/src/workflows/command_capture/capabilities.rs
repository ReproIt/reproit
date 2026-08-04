use reproit_protocol::{CaptureCapability, CaptureCapabilityKind, CaptureCompleteness};

use super::process_tree;

pub(super) fn initial(include_output: bool) -> Vec<CaptureCapability> {
    vec![
        CaptureCapability {
            capability: CaptureCapabilityKind::Commands,
            completeness: CaptureCompleteness::Complete,
            detail: None,
        },
        CaptureCapability {
            capability: CaptureCapabilityKind::ProcessTree,
            completeness: CaptureCompleteness::Partial,
            detail: Some(process_tree::capability_detail().into()),
        },
        CaptureCapability {
            capability: CaptureCapabilityKind::StandardStreams,
            completeness: if include_output {
                CaptureCompleteness::Complete
            } else {
                CaptureCompleteness::Partial
            },
            detail: (!include_output).then(|| "content was not retained".into()),
        },
        CaptureCapability {
            capability: CaptureCapabilityKind::Filesystem,
            completeness: CaptureCompleteness::Unavailable,
            detail: Some("filesystem observation is not installed".into()),
        },
        CaptureCapability {
            capability: CaptureCapabilityKind::Environment,
            completeness: CaptureCompleteness::Partial,
            detail: Some("only build identity variables were retained".into()),
        },
        CaptureCapability {
            capability: CaptureCapabilityKind::CrashDiagnostics,
            completeness: CaptureCompleteness::Partial,
            detail: Some("exit status and signal only".into()),
        },
    ]
}
