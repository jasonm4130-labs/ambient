// swift-tools-version: 6.0
// The model helper the app runs after Stop: speech-to-text and speaker
// separation on the Neural Engine through FluidAudio. See docs/adr/0020.
import PackageDescription

let package = Package(
    name: "AmbientEngine",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "ambient-engine", targets: ["AmbientEngine"])],
    dependencies: [
        // Pinned to the exact revision the benchmark on issue #168 measured.
        // v0.18.4 has both models but not the decoder fixes after it.
        .package(url: "https://github.com/FluidInference/FluidAudio.git",
                 revision: "e2ea498dafd6db9b3f18f0f644bc35312b3a7679"),
    ],
    targets: [
        // Pure logic with no model in it, so CI can test it without the
        // weights or a Neural Engine.
        .target(name: "EngineCore"),
        .executableTarget(
            name: "AmbientEngine",
            dependencies: ["EngineCore", .product(name: "FluidAudio", package: "FluidAudio")]
        ),
        .testTarget(name: "EngineCoreTests", dependencies: ["EngineCore"]),
    ]
)
