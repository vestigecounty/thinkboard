import SwiftUI

/// A horizontal, snapping pager that leaves a sliver of the neighbouring cards
/// visible at each edge — so it's obvious there are siblings to swipe to.
///
/// Built by hand (rather than `ScrollView` + `.scrollTargetBehavior`, which is
/// iOS 17+) to keep the app's iOS 16 minimum.
struct PeekPager<Content: View>: View {
    let ids: [String]
    @Binding var selection: String
    /// How much of each neighbouring card peeks in at the edges.
    let peek: CGFloat
    let spacing: CGFloat
    /// `(id, cardWidth)`. The page is a little wider than the card so the scroll
    /// indicator lands in the gap to the card's right, not on its border.
    @ViewBuilder let content: (String, CGFloat) -> Content

    @State private var drag: CGFloat = 0
    /// Locked on the first movement of a drag so a vertical scroll inside a card
    /// never nudges the pager (and a horizontal swipe never scrolls the card).
    @State private var axis: Axis?

    private var index: Int { ids.firstIndex(of: selection) ?? 0 }

    var body: some View {
        GeometryReader { geo in
            let cardWidth = max(geo.size.width - 2 * (peek + spacing), 0)
            let step = cardWidth + spacing
            // Left edge of card `index` should sit at `peek + spacing`, leaving
            // the previous card peeking on the left and the next on the right.
            let base = peek + spacing - CGFloat(index) * step
            // Each page runs `lane` past its card into the right gap; the card is
            // left-aligned, so the scroll indicator (at the page's right edge)
            // sits centered in the gap. Halving the HStack spacing keeps the total
            // card-to-card gap equal to `spacing`.
            let lane = spacing / 2

            HStack(spacing: spacing - lane) {
                ForEach(ids, id: \.self) { id in
                    content(id, cardWidth)
                        .frame(width: cardWidth + lane, height: geo.size.height, alignment: .leading)
                }
            }
            .frame(width: geo.size.width, height: geo.size.height, alignment: .leading)
            .offset(x: base + drag)
            .contentShape(Rectangle())
            // Simultaneous (not exclusive) so a swipe that starts on a tappable
            // following card still reaches the pager instead of registering as a
            // tap. The axis lock keeps it from fighting vertical scrolling.
            .simultaneousGesture(
                DragGesture(minimumDistance: 8)
                    .onChanged { value in
                        if axis == nil {
                            axis = abs(value.translation.width) >= abs(value.translation.height)
                                ? .horizontal : .vertical
                        }
                        if axis == .horizontal { drag = value.translation.width }
                    }
                    .onEnded { value in
                        defer { axis = nil }
                        guard axis == .horizontal else { drag = 0; return }
                        let projected = value.predictedEndTranslation.width
                        var target = index
                        if projected < -step / 3 {
                            target = min(index + 1, ids.count - 1)
                        } else if projected > step / 3 {
                            target = max(index - 1, 0)
                        }
                        withAnimation(.interactiveSpring(response: 0.32, dampingFraction: 0.86)) {
                            drag = 0
                            selection = ids[target]
                        }
                    }
            )
        }
    }
}
