use reproit_protocol::CaptureBatch;
use std::io::Read;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let batch: CaptureBatch = serde_json::from_slice(&input)?;
    batch.validate()?;
    println!("capture-batch-v1 valid");
    Ok(())
}
