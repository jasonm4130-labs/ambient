import EngineCore
import Foundation
import Testing

@Test func tokensMergeAtLeadingWhitespace() {
    let words = mergeTokens([
        Token(text: " Good", start: 0.0, end: 0.1, confidence: 0.8),
        Token(text: "bye", start: 0.1, end: 0.2, confidence: 1.0),
        Token(text: " world", start: 0.3, end: 0.6, confidence: 0.6),
    ])
    #expect(words.map(\.text) == ["Goodbye", "world"])
    #expect(words[0].start == 0.0)
    #expect(words[0].end == 0.2)
    #expect(abs(words[0].confidence - 0.9) < 1e-6)
    #expect(words[1].start == 0.3)
}

@Test func firstTokenWithoutSpaceStillStartsAWord() {
    let words = mergeTokens([Token(text: "ok", start: 1, end: 2, confidence: 1)])
    #expect(words == [Word(text: "ok", start: 1, end: 2, confidence: 1)])
}

@Test func noTokensNoWords() {
    #expect(mergeTokens([]) == [])
}

@Test func atomicWriteLeavesOnlyTheTarget() throws {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("engine-test-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: dir) }
    let out = dir.appendingPathComponent("asr-room.json")
    try writeAtomically(Data("one".utf8), to: out)
    try writeAtomically(Data("two".utf8), to: out)
    #expect(try String(contentsOf: out, encoding: .utf8) == "two")
    #expect(try FileManager.default.contentsOfDirectory(atPath: dir.path) == ["asr-room.json"])
}

@Test func outputRoundTrips() throws {
    let file = SpeakersFile(key: "k", model: "m", turns: [Turn(speaker: 1, start: 0.5, end: 2)])
    let back = try JSONDecoder().decode(SpeakersFile.self, from: try encode(file))
    #expect(back == file)
}

@Test func unknownFlagIsRefused() {
    #expect(throws: (any Error).self) { try Arguments(["transcribe", "--speed", "2"], known: ["models"]) }
    #expect(throws: (any Error).self) { try Arguments(["transcribe", "--models"], known: ["models"]) }
}
