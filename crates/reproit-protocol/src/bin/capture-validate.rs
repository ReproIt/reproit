use reproit_protocol::{
    compile_capture_failure, AssessmentStatus, CaptureAssessmentScope, CaptureBatch,
};
use std::io::Read;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let batch: CaptureBatch = serde_json::from_slice(&input)?;
    batch.validate()?;
    let compilation =
        compile_capture_failure(&batch, &batch.observed_at, CaptureAssessmentScope::Portable)?
            .ok_or("capture batch has no failure observation")?;
    if compilation.assessment.status != AssessmentStatus::Eligible {
        let missing = compilation
            .assessment
            .unresolved
            .iter()
            .map(|item| format!("{}: {}", item.requirement_id, item.detail))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("capture batch is not portable: {missing}").into());
    }
    println!("capture-batch-v1 portable");
    Ok(())
}
