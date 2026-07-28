use reproit_protocol::CaptureBatch;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = schemars::schema_for!(CaptureBatch);
    println!("{}", serde_json::to_string_pretty(&schema)?);
    Ok(())
}
