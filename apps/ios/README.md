# ThinkBoard iOS (SwiftUI)

A minimal first iOS front-end: pick a `.canvas` file from Files / iCloud Drive,
parse it through the Rust core, and list its nodes. It exists to shake out the
whole toolchain on device — signing, the document picker, and linking the Rust
static lib — before the real UI is built.

The Xcode project is generated from [`project.yml`](project.yml) with
[XcodeGen](https://github.com/yonaskolb/XcodeGen), so it isn't checked in.

## Build & run

```sh
# 1. Build the Rust binding (produces the git-ignored xcframework + Swift the
#    project references at ../../crates/ffi/build/).
./crates/ffi/build-xcframework.sh

# 2. Generate the Xcode project.
brew install xcodegen        # once
cd apps/ios && xcodegen      # regenerate whenever project.yml changes

# 3. Open and run.
open ThinkBoard.xcodeproj
```

In Xcode: select the **ThinkBoard** target → **Signing & Capabilities** → pick
your Team (a free personal Apple ID works; the app expires after 7 days). Choose
your iPhone (or a simulator) and Run.

Tap **Open Folder** and choose the folder holding your canvas (opening the
folder — not a single `.canvas` — is what lets the app read the images that file
nodes reference). Pick a canvas if the folder has more than one, then explore it
in the structure view: swipe between sibling cards, scroll down to a card's
following nodes, tap one to descend, back-swipe to ascend.

## Notes

- Re-run `build-xcframework.sh` after any change to `crates/ffi` or
  `crates/core`, then rebuild in Xcode.
- Re-run `xcodegen` only after editing `project.yml`.
- The one non-obvious build setting is `OTHER_LDFLAGS: -liconv`, required by the
  Rust static lib (see `project.yml`).
