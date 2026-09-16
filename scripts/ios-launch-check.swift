// CI-only check of the actual simulator display, using macOS's built-in OCR.
// A living app process can still be stuck on a blank native launch screen.
import Foundation
import Vision

guard CommandLine.arguments.count == 2 else {
    fputs("usage: ios-launch-check <screenshot.png>\n", stderr)
    exit(2)
}

do {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.recognitionLanguages = ["en-US"]
    let handler = VNImageRequestHandler(
        url: URL(fileURLWithPath: CommandLine.arguments[1]), options: [:]
    )
    try handler.perform([request])
    let lines = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }
    print(lines.joined(separator: "\n"))
    let text = lines.joined(separator: " ").lowercased()
        .split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
    exit(text.contains("connect to a computer") ? 0 : 1)
} catch {
    fputs("Could not read simulator screenshot: \(error)\n", stderr)
    exit(2)
}
