// C entry point for the iOS host app; reuses the generated main.rs as-is.
include!("main.rs");

#[no_mangle]
pub extern "C" fn recomp_run() -> i32 {
    match main() {
        Ok(()) => 0,
        Err(err) => {
            println!("runtime error: {err:?}");
            1
        }
    }
}
