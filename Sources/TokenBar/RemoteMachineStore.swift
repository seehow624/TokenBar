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
    /// True when the text above includes a root that failed outright, rather
    /// than only a transient partial-transfer warning. Drives the settings
    /// row's red-vs-orange treatment.
    private(set) var lastSyncHadFailure = false
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

    /// Whether `startScheduler()` should arm the loop for the given task state.
    /// Split out so the smoke check can pin the launch case (no task yet) that
    /// the previous inverted guard rejected.
    nonisolated static func shouldStartScheduler(existing: Task<Void, Never>?) -> Bool {
        guard let existing else { return true }
        return existing.isCancelled
    }

    /// Start the hourly loop. Called once at launch (and restartable after a
    /// settings change); never started in demo mode.
    func startScheduler() {
        // Start only when no live task exists. The previous spelling,
        // `guard !(syncTask?.isCancelled ?? true)`, inverted this: a nil task
        // coalesced to `true`, so the very first call (app launch) returned
        // early and the hourly loop never started — only the manual "Sync now"
        // button ever synced a mirror.
        guard Self.shouldStartScheduler(existing: syncTask) else { return }
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

    /// One root's rsync outcome. `error == nil` means the root mirrored cleanly.
    struct RootSyncOutcome: Sendable {
        let remotePath: String
        let error: String?
        /// True for rsync's partial-transfer codes (23/24): source files
        /// vanished mid-run (a live SQLite checkpointing its WAL sidecars) or a
        /// few files failed. The mirror did advance and the next pass repairs
        /// it, so this warns instead of vetoing the clean-sync stamp.
        let isTransient: Bool
    }

    /// rsync one machine's agent roots into its mirror home.
    ///
    /// The rsync loop blocks (Process + waitUntilExit) and a first sync moves
    /// gigabytes, so it runs detached from the main actor: the popover keeps
    /// drawing while the mirror catches up.
    func sync(_ machine: RemoteMachine) async {
        guard let mirror = mirrorHomeDirectory(for: machine.name) else { return }
        let home = mirror.appendingPathComponent("home", isDirectory: true)
        try? FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)

        let outcomes = await Task.detached(priority: .utility) {
            Self.mirrorRoots(of: machine, into: home)
        }.value

        let messages = outcomes.compactMap { outcome -> String? in
            guard let error = outcome.error else { return nil }
            return "\(outcome.isTransient ? "warning" : "error"): \(machine.name): \(outcome.remotePath): \(error)"
        }
        lastSyncHadFailure = outcomes.contains { $0.error != nil && !$0.isTransient }
        lastSyncError = messages.isEmpty ? nil : messages.joined(separator: "\n")

        guard let index = machines.firstIndex(where: { $0.name == machine.name }) else { return }
        let now = UInt64(Date().timeIntervalSince1970)
        if !lastSyncHadFailure {
            machines[index].lastSyncedAt = now
            machines[index].lastPartialSyncedAt = nil
        } else if outcomes.contains(where: { $0.error == nil }) {
            // Some roots landed, so the mirror did advance even though one root
            // failed. Keep `lastSyncedAt` as "last fully clean sync" and record
            // the partial one separately, so Settings cannot read as frozen
            // while data is actually flowing.
            machines[index].lastPartialSyncedAt = now
        }
        save()
    }

    /// Blocking rsync of every mirrored root. Never call this on the main actor.
    nonisolated static func mirrorRoots(of machine: RemoteMachine, into home: URL) -> [RootSyncOutcome] {
        var outcomes: [RootSyncOutcome] = []
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
            let errors = Pipe()
            process.standardError = errors
            // Discard stdout: nothing reads it, and a full 64 KiB pipe buffer
            // would deadlock rsync against waitUntilExit().
            process.standardOutput = FileHandle.nullDevice
            do {
                try process.run()
                // Drain stderr to EOF before waiting: EOF is the child closing
                // the pipe, so this cannot deadlock on a full stderr buffer.
                let data = errors.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                let message = String(data: data, encoding: .utf8)?
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                if process.terminationStatus == 0 {
                    outcomes.append(RootSyncOutcome(remotePath: root.remotePath, error: nil, isTransient: false))
                } else {
                    let detail = message.flatMap { $0.isEmpty ? nil : $0 }
                        ?? "rsync exited \(process.terminationStatus)"
                    let transient = process.terminationStatus == 23 || process.terminationStatus == 24
                    outcomes.append(RootSyncOutcome(
                        remotePath: root.remotePath, error: detail, isTransient: transient))
                }
            } catch {
                outcomes.append(RootSyncOutcome(
                    remotePath: root.remotePath, error: error.localizedDescription, isTransient: false))
            }
        }
        return outcomes
    }
}
