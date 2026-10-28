# iOS host app for recompiled code

Minimal SwiftUI app that links a recompiled program as a static library and shows its
stdout on screen. It runs the `out/arm-native` output of the homebrew lift flow.

1. Produce `out/arm-native` with `recomp run` (see `docs/DEVELOPMENT.md`), passing
   `--runtime-path crates/recomp-runtime`.
2. Turn it into a static library:

   ```sh
   cd out/arm-native
   printf '\n[workspace]\n\n[lib]\npath = "src/lib.rs"\ncrate-type = ["staticlib"]\n' >> Cargo.toml
   cp ../../examples/ios-app/recomp_lib.rs src/lib.rs
   cargo build --release --lib --target aarch64-apple-ios
   ```

   The library name follows the package name (`librecomp_minimal_sample.a` for the
   minimal title config); update `OTHER_LDFLAGS` in the project if yours differs.
3. Build and install (set `DEVELOPMENT_TEAM` / `PRODUCT_BUNDLE_IDENTIFIER` to your own):

   ```sh
   cd examples/ios-app
   xcodebuild -project RecompDemo.xcodeproj -scheme RecompDemo \
     -destination 'id=<device-udid>' -derivedDataPath dd -allowProvisioningUpdates build
   xcrun devicectl device install app --device <device-udid> dd/Build/Products/Debug-iphoneos/RecompDemo.app
   ```

Tap **Run recompiled code** to call `recomp_run()` and display its output.
