fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/agent.proto");
    tonic_build::compile_protos("proto/agent.proto")?;
    Ok(())
}
