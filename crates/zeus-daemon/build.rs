//! Keep packaged MSVC executables independent of the VC redistributable.
fn main() {
    static_vcruntime::metabuild();
}
