import Foundation

public struct TranscriptFile: Codable, Equatable, Sendable {
    /// The app reuses this file only when `key` matches, so a changed track or
    /// model never reuses stale output.
    public var key: String
    public var model: String
    public var durationSeconds: Double
    public var words: [Word]

    public init(key: String, model: String, durationSeconds: Double, words: [Word]) {
        self.key = key
        self.model = model
        self.durationSeconds = durationSeconds
        self.words = words
    }
}

public struct Word: Codable, Equatable, Sendable {
    public var text: String
    public var start: Double
    public var end: Double
    public var confidence: Float

    public init(text: String, start: Double, end: Double, confidence: Float) {
        self.text = text
        self.start = start
        self.end = end
        self.confidence = confidence
    }
}

public struct SpeakersFile: Codable, Equatable, Sendable {
    public var key: String
    public var model: String
    public var turns: [Turn]

    public init(key: String, model: String, turns: [Turn]) {
        self.key = key
        self.model = model
        self.turns = turns
    }
}

public struct Turn: Codable, Equatable, Sendable {
    public var speaker: Int
    public var start: Double
    public var end: Double

    public init(speaker: Int, start: Double, end: Double) {
        self.speaker = speaker
        self.start = start
        self.end = end
    }
}

public struct Token: Sendable {
    public var text: String
    public var start: Double
    public var end: Double
    public var confidence: Float

    public init(text: String, start: Double, end: Double, confidence: Float) {
        self.text = text
        self.start = start
        self.end = end
        self.confidence = confidence
    }
}

/// The FluidAudio CLI's word-joining rule, as benchmarked in issue #168.
public func mergeTokens(_ tokens: [Token]) -> [Word] {
    var words: [Word] = []
    var text = ""
    var start: Double?
    var end = 0.0
    var confidences: [Float] = []

    func flush() {
        if !text.isEmpty, let s = start {
            let c = confidences.isEmpty ? 0 : confidences.reduce(0, +) / Float(confidences.count)
            words.append(Word(text: text, start: s, end: end, confidence: c))
        }
    }

    for t in tokens {
        if let first = t.text.first, first.isWhitespace {
            flush()
            text = t.text.trimmingCharacters(in: .whitespacesAndNewlines)
            start = t.start
            confidences = []
        } else {
            if start == nil { start = t.start }
            text += t.text
        }
        end = t.end
        confidences.append(t.confidence)
    }
    flush()
    return words
}

/// Temp file then rename, so a reader sees nothing or all of it; a helper killed
/// mid-write leaves only the temp file.
public func writeAtomically(_ data: Data, to url: URL) throws {
    let tmp = url.deletingLastPathComponent()
        .appendingPathComponent(".\(url.lastPathComponent).\(getpid()).tmp")
    try data.write(to: tmp)
    if rename(tmp.path, url.path) != 0 {
        let err = String(cString: strerror(errno))
        try? FileManager.default.removeItem(at: tmp)
        throw EngineError.message("could not move \(tmp.path) into place: \(err)")
    }
}

public func encode<T: Encodable>(_ value: T) throws -> Data {
    let e = JSONEncoder()
    e.outputFormatting = [.sortedKeys]
    return try e.encode(value)
}

public enum EngineError: Error, CustomStringConvertible {
    case message(String)
    case usage(String)

    public var description: String {
        switch self {
        case .message(let m): return m
        case .usage(let m): return m
        }
    }
}

public struct Arguments {
    public var command: String
    public var flags: [String: String]

    public init(_ argv: [String], known: Set<String>) throws {
        guard let command = argv.first else { throw EngineError.usage(usage) }
        self.command = command
        var flags: [String: String] = [:]
        var i = 1
        while i < argv.count {
            let a = argv[i]
            guard a.hasPrefix("--"), known.contains(String(a.dropFirst(2))) else {
                throw EngineError.usage("unknown argument \(a)\n\(usage)")
            }
            guard i + 1 < argv.count else { throw EngineError.usage("\(a) needs a value") }
            flags[String(a.dropFirst(2))] = argv[i + 1]
            i += 2
        }
        self.flags = flags
    }

    public func require(_ name: String) throws -> String {
        guard let v = flags[name] else { throw EngineError.usage("--\(name) is required\n\(usage)") }
        return v
    }
}

public let usage = """
    usage:
      ambient-engine fetch --models <dir>
      ambient-engine transcribe --models <dir> --audio <wav> --out <json> --key <string>
      ambient-engine diarize --models <dir> --audio <wav> --out <json> --key <string>

    stdout carries one line per event: `progress <0..1>`, and `error <message>`
    before a non-zero exit.
    """
