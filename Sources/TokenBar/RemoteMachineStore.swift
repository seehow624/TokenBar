import Foundation
import Observation
import TokenBarCore

/// Owns the remote-machine list, the rsync mirror sync, and the hourly
/// schedule. The mirror is a fake home under Application Support
/// (`RemoteMachines/<name>/home`) holding the remote machine's `.claude`,
/// `.codex`, `.hermes`, and `.local/share/opencode` trees, which the vendored
/// scanner reads through the `tb_*_remote` FFI entry points.
@MainActor @Observable
final class RemoteMachineStore {
    static let shared = RemoteMachineStore()
    static let syncIntervalSecs: TimeInterval = 3600

    private static let defaultsKey = "tokenbar.remoteMachines"
    /// Feature flag while this ships without UI defaults: when no machines are
    /// configured, the mirror stays off and the store is a no-op.
    static let isFeatureAvailable = true

    private(set) var machines: [RemoteMachine] = []
    private(set) var lastSyncError: String?
    private var syncTask: Task<Void, Never>?
    private var nextScheduledSync = Date.distantPast

    private init() {
        loadFromDefaults()
    }

    /// Enabled machines, sorted by name.
    var enabledMachines: [RemoteMachine] {
        machines.filter(\.isEnabled).sorted { $0.name < $1.name }
    }

    // MARK: - Persistence

    private func loadFromDefaults() {
        guard let data = UserDefaults.standard.data(forKey: Self.defaultsKey),
              let decoded = try? JSONDecoder().decode([RemoteMachine].self, from: data)
        else { return }
        machines = decoded
    }

    func save() {
        guard let data = try? JSONEncoder().encode(machines) else { return }
        UserDefaults.standard.set(data, forKey: Self.defaultsKey)
    }

    // MARK: - Mutations

    func upsert(_ machine: RemoteMachine) {
        if let index = machines.firstIndex(where: { $0.name == machine.name }) {
            machines[index] = machine
        } else {
            machines.append(machine)
        }
        save()
    }

    func remove(named name: String) {
        machines.removeAll { $0.name == name }
        // Drop the mirror so a re-added machine with the same name starts
        // clean (and so stale data cannot leak into a different host).
        if let mirror = mirrorHomeDirectory(for: name) {
            try? FileManager.default.removeItem(at: mirror)
        }
        save()
    }

    // MARK: - Mirror paths

    func mirrorHomeDirectory(for name: String) -> URL? {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first?
            .appendingPathComponent("TokenBar/RemoteMachines/\(name)", isDirectory: true)
        return base
    }

    /// Whether a mirror for `name` exists with at least one scanned source.
    func hasMirroredData(for name: String) -> Bool {
        guard let mirror = mirrorHomeDirectory(for: name) else { return false }
        let probes = [
            mirror.appendingPathComponent("home/.claude/projects"),
            mirror.appendingPathComponent("home/.codex/sessions"),
            mirror.appendingPathComponent("home/.hermes"),
            mirror.appendingPathComponent("home/.local/share/opencode"),
        ]
        return probes.contains { FileManager.default.fileExists(atPath: $0.path) }
    }

    // MARK: - Scheduling

    /// Start the hourly loop. Called once at launch (and restartable after a
    /// settings change); never started in demo mode.
    func startScheduler() {
        guard !(syncTask?.isCancelled ?? true) else { return }
        syncTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                // First run: sync immediately if no machine has ever synced,
                // otherwise wait for the next hourly boundary.
                if Date() >= self.nextScheduledSync {
                    await self.syncAll()
                    self.nextScheduledSync = Date().addingTimeInterval(Self.syncIntervalSecs)
                }
                try? await Task.sleep(nanoseconds: UInt64(60 * 1_000_000_000))
            }
        }
    }

    func stopScheduler() {
        syncTask?.cancel()
        syncTask = nil
    }

    /// Force a sync of every enabled machine now.
    func syncAll() async {
        guard Self.isFeatureAvailable else { return }
        let enabled = enabledMachines
        guard !enabled.isEmpty else { return }
        await withTaskGroup(of: Void.self) { group in
            for machine in enabled {
                group.addTask { await self.sync(machine) }
            }
        }
    }

    /// rsync one machine's agent roots into its mirror home.
    func sync(_ machine: RemoteMachine) async {
        guard let mirror = mirrorHomeDirectory(for: machine.name) else { return }
        let home = mirror.appendingPathComponent("home", isDirectory: true)
        try? FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)

        var results: [String] = []
        var anyFailure = false
        for root in RemoteMachine.mirroredRoots {
            let destination = home.appendingPathComponent(root.mirrorRelative, isDirectory: true)
            try? FileManager.default.createDirectory(at: destination, withIntermediateDirectories: true)
            // rsync -az --relative strips nothing; instead we cd into the
            // remote home via ssh so the remote path is relative to it, and
            // keep the local destination explicit.
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/rsync")
            process.arguments = [
                "-az", "--delete",
                "-e", "ssh -o BatchMode=yes -o ConnectTimeout=10",
                "\(machine.sshDestination):\(root.remotePath)/",
                destination.path + "/",
            ]
            let pipe = Pipe()
            process.standardError = pipe
            process.standardOutput = Pipe()
            do {
                try process.run()
                process.waitUntilExit()
                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                if process.terminationStatus != 0 {
                    anyFailure = true
                    results.append("\(root.remotePath): \(String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? "rsync failed")")
                }
            } catch {
                anyFailure = true
                results.append("\(root.remotePath): \(error.localizedDescription)")
            }
        }

        if anyFailure {
            lastSyncError = results.joined(separator: "\n")
        } else {
            lastSyncError = nil
            if let index = machines.firstIndex(where: { $0.name == machine.name }) {
                machines[index].lastSyncedAt = UInt64(Date().timeIntervalSince1970)
                save()
            }
        }
    }
}
