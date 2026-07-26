# thinkboard-ffi

The mobile binding layer: it wraps [`thinkboard-core`](../core) in a
[uniffi](https://mozilla.github.io/uniffi-rs/)-generated FFI so a Swift / iOS
front-end can call the ThinkBoard document engine. The core crate stays
binding-free; everything FFI lives here.

## What it exposes

```
coreVersion() -> String                             // smoke test
parseMarkdown(source:) -> [MarkdownSpan]             // styled runs for rendering

Document.parse(json:) throws -> Document             // parse a .canvas
  .nodes()  -> [Node]
  .edges()  -> [Edge]
  .toJson() throws -> String                         // lossless round-trip
  .layout(direction:rankGap:nodeGap:) -> [LayoutPosition]

  // editing (mutates in place; follow with toJson() to persist)
  .addTextNode(text:x:y:width:height:) -> String     // new node id
  .setNodeText(id:text:) -> Bool
  .moveNode(id:x:y:) -> Bool
  .removeNode(id:) -> Bool                            // also drops its edges
  .addEdge(fromNode:fromSide:toNode:toSide:) -> String
  .removeEdge(id:) -> Bool
  .applyLayout(direction:rankGap:nodeGap:)            // write positions back
```

`Document` is an opaque, reference-counted object that owns the full parsed
model — including fields ThinkBoard doesn't model — so `toJson()` never drops
another tool's data. `Node` / `Edge` / `MarkdownSpan` / `LayoutPosition` are
plain value structs for the UI.

- `parseMarkdown` turns note text into styled spans (bold/italic/code, heading
  level, links, and per-token syntax classes for code blocks).
- `layout` returns computed positions without mutating; `applyLayout` writes
  them back. The editing methods live on `CanvasDocument` in the core crate, so
  every front-end mutates a canvas the same way.
- After any edit, call `toJson()` and write the result back to the file / iCloud
  Drive.

Coming next: the AI copilot (already compiled into the library —
`reqwest`/`rustls` cross-compile cleanly for iOS — just not yet surfaced through
the FFI).

## Build the XCFramework

Prerequisites (one time):

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
# Xcode with the iOS SDK (for xcodebuild -create-xcframework)
```

Then:

```sh
./crates/ffi/build-xcframework.sh          # release (default)
./crates/ffi/build-xcframework.sh debug    # faster, larger, for iterating
```

This produces (git-ignored, under `crates/ffi/build/`):

- `ThinkBoardFFI.xcframework` — device (arm64) + simulator (arm64) static libs
- `ThinkBoardFFI.swift` — the generated Swift API

## Use it in Xcode

1. Drag `ThinkBoardFFI.xcframework` into your project (check *Embed & Sign*, or
   for a static lib just add it to *Frameworks, Libraries, and Embedded
   Content*).
2. Add `ThinkBoardFFI.swift` to your app target.
3. Call it:

```swift
import Foundation

let version = coreVersion()

let json = try String(contentsOf: canvasFileURL, encoding: .utf8)
let doc = try Document.parse(json: json)
for node in doc.nodes() {
    print(node.id, node.text ?? "")
}
let saved = try doc.toJson()   // write back to the file / iCloud Drive
```

## Regenerating just the Swift bindings

The `build-xcframework.sh` script does this for you, but to regenerate against a
host build directly:

```sh
cargo build -p thinkboard-ffi
cargo run -p thinkboard-ffi --bin uniffi-bindgen -- \
  generate --library target/debug/libthinkboard_ffi.dylib \
  --language swift --out-dir <out>
```
