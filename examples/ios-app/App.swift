import SwiftUI

@_silgen_name("recomp_run") func recomp_run() -> Int32

@main
struct RecompDemoApp: App {
    var body: some Scene {
        WindowGroup { ContentView() }
    }
}

struct ContentView: View {
    @State private var output = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("SwitchRecomp Demo").font(.title.bold())
            Text("AArch64 → Rust → native iOS").foregroundStyle(.secondary)
            Button("Run recompiled code") { output = runRecompiled() }
                .buttonStyle(.borderedProminent)
            ScrollView {
                Text(output)
                    .font(.system(.body, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .padding()
    }
}

// ponytail: output goes through a pipe drained after the run, so it is capped by the
// pipe buffer (~64 KB); read on a background thread if programs get chatty.
func runRecompiled() -> String {
    let pipe = Pipe()
    fflush(stdout)
    let saved = dup(STDOUT_FILENO)
    dup2(pipe.fileHandleForWriting.fileDescriptor, STDOUT_FILENO)
    let code = recomp_run()
    fflush(stdout)
    dup2(saved, STDOUT_FILENO)
    close(saved)
    try? pipe.fileHandleForWriting.close()
    let text = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
    return text + "\nexit code: \(code)"
}
