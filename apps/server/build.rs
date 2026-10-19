fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/android/emulator_controller.proto");
    let descriptors = protox::compile(["android/emulator_controller.proto"], ["proto"])?;
    tonic_prost_build::configure()
        .build_server(true) // the in-process fake controller in tests
        .build_client(true)
        .compile_fds(descriptors)?;
    Ok(())
}
