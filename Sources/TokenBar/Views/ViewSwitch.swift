import SwiftUI

/// The six-lens tab row under the header, port of ViewSwitch.tsx.
struct ViewSwitch: View {
    @Binding var active: AppView

    var body: some View {
        HStack(spacing: 2) {
            ForEach(AppView.allCases, id: \.self) { view in
                Button(view.label) { active = view }
                    .buttonStyle(.plain)
                    .font(.caption.weight(active == view ? .semibold : .regular))
                    .foregroundStyle(active == view ? .primary : .secondary)
                    .lineLimit(1)
                    .minimumScaleFactor(0.75)
                    .padding(.horizontal, 4)
                    .padding(.vertical, 4)
                    .frame(maxWidth: .infinity)
                    .background { pillBackground(active: active == view) }
            }
        }
        .padding(2)
        .glassCard(cornerRadius: 8)
    }

    /// Selected pill: interactive Liquid Glass on macOS 26 (visually reacts
    /// to hover/press), a flat quaternary fill on the macOS 14 fallback.
    @ViewBuilder private func pillBackground(active: Bool) -> some View {
        if active {
            if #available(macOS 26.0, *) {
                Color.clear.glassEffect(.regular.interactive(), in: .rect(cornerRadius: 6))
            } else {
                RoundedRectangle(cornerRadius: 6).fill(.quaternary)
            }
        }
    }
}
