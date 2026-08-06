import AppKit
import SwiftUI
import TokenBarCore

/// Remote-machine mirror management: add an SSH destination, see sync status,
/// and force a sync. Mirrored agent data appears in the popover through the
/// machine-scope menu (this machine / a remote / all machines).
struct RemoteMachinesSection: View {
    @State private var store = RemoteMachineStore.shared
    @State private var newName = ""
    @State private var newDestination = ""
    @State private var syncing = false
    @State private var lastAction: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(store.machines) { machine in
                machineRow(machine)
            }

            HStack(spacing: 6) {
                TextField("Name", text: $newName)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 72)
                TextField("user@host", text: $newDestination)
                    .textFieldStyle(.roundedBorder)
                Button("Add") { addMachine() }
                    .controlSize(.small)
                    .disabled(newName.trimmingCharacters(in: .whitespaces).isEmpty
                        || newDestination.trimmingCharacters(in: .whitespaces).isEmpty)
            }

            if let error = store.lastSyncError {
                Text(error)
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .lineLimit(3)
            }
            if let lastAction {
                Text(lastAction)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            if syncing {
                ProgressView()
                    .controlSize(.small)
            }
            Text("Syncs every hour over SSH (rsync + your default SSH keys). "
                + "Claude, Codex, Hermes and OpenCode data are mirrored; "
                + "usage shows under the machine menu in the popover.")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }

    private func machineRow(_ machine: RemoteMachine) -> some View {
        HStack(spacing: 8) {
            Toggle("", isOn: Binding(
                get: { store.machines.first(where: { $0.name == machine.name })?.isEnabled ?? false },
                set: { next in
                    store.upsert(RemoteMachine(
                        name: machine.name,
                        sshDestination: machine.sshDestination,
                        isEnabled: next,
                        lastSyncedAt: machine.lastSyncedAt))
                }))
            .labelsHidden()
            .toggleStyle(.switch)
            .controlSize(.mini)

            VStack(alignment: .leading, spacing: 2) {
                Text(machine.name)
                    .font(.caption.weight(.semibold))
                Text(machine.sshDestination)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            if let synced = machine.lastSyncedAt {
                Text(syncedLabel(synced))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            } else {
                Text("never synced")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            Button("Sync now") {
                Task { await sync(machine) }
            }
            .controlSize(.mini)
            .disabled(syncing)
            Button {
                store.remove(named: machine.name)
            } label: {
                Image(systemName: "trash")
                    .font(.caption2)
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .help("Remove this machine and its mirror")
        }
        .padding(8)
        .background(Color.primary.opacity(0.04), in: RoundedRectangle(cornerRadius: 8))
    }

    private func addMachine() {
        let name = newName.trimmingCharacters(in: .whitespaces)
        let destination = newDestination.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty, !destination.isEmpty else { return }
        store.upsert(RemoteMachine(name: name, sshDestination: destination))
        newName = ""
        newDestination = ""
        lastAction = "Added \(name) — first sync starts now."
        Task { await store.syncAll() }
    }

    private func sync(_ machine: RemoteMachine) async {
        syncing = true
        defer { syncing = false }
        await store.sync(machine)
        lastAction = store.lastSyncError == nil ? "Synced \(machine.name)." : nil
    }

    private func syncedLabel(_ seconds: UInt64) -> String {
        let interval = TimeInterval(seconds)
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .short
        return "synced \(formatter.localizedString(for: Date(timeIntervalSince1970: interval), relativeTo: Date()))"
    }
}
