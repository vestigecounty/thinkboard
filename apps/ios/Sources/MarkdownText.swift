import SwiftUI

/// Renders note text as styled SwiftUI text, using the core's markdown parser
/// (`parseMarkdown`) so the phone and desktop agree on how a note is broken into
/// styled runs. Handles bold/italic/monospace, heading sizes, and links.
struct MarkdownText: View {
    let source: String

    var body: some View {
        // No `.textSelection` here: it can swallow the tap that descends into a
        // following card.
        Text(Self.attributed(source))
            .tint(.accentColor)
    }

    static func attributed(_ source: String) -> AttributedString {
        var out = AttributedString()
        for span in parseMarkdown(source: source) {
            var run = AttributedString(span.text)
            var font = headingFont(span.heading)
            if span.code { font = font.monospaced() }
            if span.bold { font = font.bold() }
            if span.italic { font = font.italic() }
            run.font = font
            if span.link {
                run.foregroundColor = .accentColor
                if let url = span.url, let parsed = URL(string: url) {
                    run.link = parsed
                }
            }
            out += run
        }
        return out
    }

    /// Body text for level 0; progressively larger for headings 1–6.
    private static func headingFont(_ level: UInt8) -> Font {
        switch level {
        case 1: return .largeTitle
        case 2: return .title
        case 3: return .title2
        case 4: return .title3
        case 5: return .headline
        case 6: return .subheadline
        default: return .body
        }
    }
}
