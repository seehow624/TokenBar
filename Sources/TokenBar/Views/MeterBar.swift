import SwiftUI

/// The single meter language for every horizontal amount bar in the app.
/// One height, one shape, one track/fill treatment — cards must not draw
/// their own bars (that's how the 4/6/8pt drift happened).
struct MeterBar: View {
    static let height: CGFloat = 6
    static let trackOpacity: Double = 0.55
    static let fillOpacity: Double = 0.85

    /// 0...1; clamped internally.
    var fraction: Double
    var color: Color
    var marker: Marker?

    /// Pace marker (expected-usage tick on the limits card).
    struct Marker {
        var fraction: Double
        var isDeficit: Bool
        var help: String
    }

    init(fraction: Double, color: Color, marker: Marker? = nil) {
        self.fraction = fraction
        self.color = color
        self.marker = marker
    }

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary.opacity(Self.trackOpacity))
                Capsule()
                    .fill(color.opacity(Self.fillOpacity))
                    .frame(width: geo.size.width * min(max(fraction, 0), 1))
                if let marker {
                    // The tick stays inside the capsule: a taller tick makes
                    // marked bars read thicker than unmarked ones.
                    RoundedRectangle(cornerRadius: 0.75)
                        .fill(marker.isDeficit ? Color.orange : Color.secondary)
                        .frame(width: 1.5, height: geo.size.height)
                        .offset(x: geo.size.width * min(max(marker.fraction, 0), 1) - 0.75)
                        .help(marker.help)
                }
            }
        }
        .frame(height: Self.height)
    }
}
