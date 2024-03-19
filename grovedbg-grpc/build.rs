fn main() {
    println!("cargo:rerun-if-changed=./protos");

    tonic_build::compile_protos("proto/grovedbg.proto")
        .unwrap_or_else(|e| panic!("Failed to compile protos {:?}", e));
}
