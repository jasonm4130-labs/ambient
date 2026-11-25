// ambient-engine: the app's model helper. One stage, one track, one process:
// the app runs it after Stop, reads the file it writes, and the process exits
// and gives back every page the models used. See docs/adr/0020.
import AVFoundation
import CoreML
import EngineCore
import FluidAudio
import Foundation

setvbuf(stdout, nil, _IOLBF, 0)

/// Folder names inside `--models`, as FluidAudio lays them out.
let asrFolder = "parakeet-ultra"
let diarFolder = "nemotron-3-diarization"
/// Written into every output so the app can tell which models made it.
let asrModel = "parakeet-ultra/fluidaudio-e2ea498"
let diarModel = "nemotron3-fast32/fluidaudio-e2ea498"

func say(_ line: String) {
    print(line)
}

func progress(_ fraction: Double) {
    say(String(format: "progress %.3f", min(max(fraction, 0), 1)))
}

func requireFiles(_ models: URL) throws {
    let asr = models.appendingPathComponent(asrFolder)
    let diar = models.appendingPathComponent(diarFolder)
    guard AsrModels.modelsExist(at: asr, version: .ultra) else {
        throw EngineError.message("speech model missing at \(asr.path) — run ./fetch-models.sh")
    }
    guard FileManager.default.fileExists(atPath: diar.appendingPathComponent("monolithic/v2").path) else {
        throw EngineError.message("speaker model missing at \(diar.path) — run ./fetch-models.sh")
    }
}

func fetch(_ models: URL) async throws {
    try FileManager.default.createDirectory(at: models, withIntermediateDirectories: true)
    progress(0)
    _ = try await AsrModels.download(to: models.appendingPathComponent(asrFolder), version: .ultra)
    progress(0.5)
    // Downloads only what is missing, then compiles and loads once, which is
    // what proves the copy on disk is whole.
    _ = try await Nemotron3Models.loadFromHuggingFace(config: .fast32, cacheDirectory: models)
    progress(1)
}

func transcribe(models: URL, audio: URL, out: URL, key: String) async throws {
    try requireFiles(models)
    progress(0)
    let loaded = try AsrModels.loadLocal(
        from: models.appendingPathComponent(asrFolder), version: .ultra)
    // The library's defaults with Ultra's decoder shape: the configuration
    // the CLI ran for the benchmark on issue #168.
    let config = ASRConfig(
        tdtConfig: TdtConfig(blankId: AsrModelVersion.ultra.blankId),
        encoderHiddenSize: AsrModelVersion.ultra.encoderHiddenSize)
    let manager = AsrManager(config: config)
    try await manager.loadModels(loaded)
    var state = TdtDecoderState.make(decoderLayers: await manager.decoderLayerCount)
    // Files over the library's threshold are decoded from disk in windows, so
    // memory stays flat however long the meeting was.
    let result = try await manager.transcribe(audio, decoderState: &state)
    let tokens = (result.tokenTimings ?? []).map {
        Token(text: $0.token, start: $0.startTime, end: $0.endTime, confidence: $0.confidence)
    }
    let file = TranscriptFile(
        key: key, model: asrModel, durationSeconds: result.duration, words: mergeTokens(tokens))
    try writeAtomically(try encode(file), to: out)
    progress(1)
}

func diarize(models: URL, audio: URL, out: URL, key: String) async throws {
    try requireFiles(models)
    progress(0)
    let config = Nemotron3Config.fast32
    let loaded = try await Nemotron3Models.loadFromHuggingFace(config: config, cacheDirectory: models)
    let diarizer = Nemotron3Diarizer(config: config, models: loaded)

    let file = try AVAudioFile(forReading: audio)
    let format = file.processingFormat
    guard format.sampleRate == 16000, format.channelCount == 1 else {
        throw EngineError.message(
            "\(audio.path) is \(format.sampleRate) Hz × \(format.channelCount); expected 16 kHz mono")
    }
    // Fed through the streaming path in 100 ms pieces, read from disk as it
    // goes: the speaker model's memory no longer grows with the meeting
    // (593 MB against 891 MB at 49.5 min on AMI, same speaker counts).
    let piece: AVAudioFrameCount = 1600
    guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: piece) else {
        throw EngineError.message("could not allocate an audio buffer")
    }
    var probabilities: [Float] = []
    var frames = 0
    let total = max(Double(file.length), 1)
    var lastReported = 0.0
    diarizer.reset()
    while file.framePosition < file.length {
        try file.read(into: buffer, frameCount: piece)
        guard let channel = buffer.floatChannelData?[0], buffer.frameLength > 0 else { break }
        diarizer.appendAudio(Array(UnsafeBufferPointer(start: channel, count: Int(buffer.frameLength))))
        for r in try diarizer.processBufferedAudio() {
            probabilities.append(contentsOf: r.probabilities)
            frames += r.frameCount
        }
        let done = Double(file.framePosition) / total
        if done - lastReported >= 0.05 {
            progress(done)
            lastReported = done
        }
    }
    for r in try diarizer.finishStream() {
        probabilities.append(contentsOf: r.probabilities)
        frames += r.frameCount
    }
    let turns = Nemotron3Diarizer.segments(probabilities: probabilities, frameCount: frames)
        .map { Turn(speaker: $0.speakerIndex, start: Double($0.startSeconds), end: Double($0.endSeconds)) }
    try writeAtomically(try encode(SpeakersFile(key: key, model: diarModel, turns: turns)), to: out)
    progress(1)
}

do {
    let args = try Arguments(
        Array(CommandLine.arguments.dropFirst()), known: ["models", "audio", "out", "key"])
    let models = URL(fileURLWithPath: try args.require("models"))
    switch args.command {
    case "fetch":
        try await fetch(models)
    case "transcribe", "diarize":
        let audio = URL(fileURLWithPath: try args.require("audio"))
        let out = URL(fileURLWithPath: try args.require("out"))
        let key = try args.require("key")
        if args.command == "transcribe" {
            try await transcribe(models: models, audio: audio, out: out, key: key)
        } else {
            try await diarize(models: models, audio: audio, out: out, key: key)
        }
    default:
        throw EngineError.usage("unknown command \(args.command)\n\(usage)")
    }
    exit(0)
} catch let e as EngineError {
    say("error \(e.description.replacingOccurrences(of: "\n", with: " "))")
    if case .usage = e { exit(64) }
    exit(1)
} catch {
    say("error \(String(describing: error).replacingOccurrences(of: "\n", with: " "))")
    exit(1)
}
