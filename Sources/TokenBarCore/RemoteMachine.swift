import Foundation

// Remote-machine mirror support. A second machine's agent data is rsynced
// into a fake home under Application Support (see RemoteMachineSync), and
// TokenBarCore fetches a separate pre-aggregated payload per machine through
// the tb_*_remote FFI entry points. This file owns the machine model and the
// merge that combines the local payload with remote payloads.

/// A configured remote machine whose agent data is mirrored locally.
public struct RemoteMachine: Codable, Identifiable, Sendable, Equatable {
    /// Display name (e.g. "mini").
    public var name: String
    /// SSH destination in `user@host` form.
    public var sshDestination: String
    /// Whether the mirror sync is enabled and its payload is shown.
    public var isEnabled: Bool
    /// Unix timestamp (seconds) of the last successful sync.
    public var lastSyncedAt: UInt64?

    public var id: String { name }

    public init(name: String, sshDestination: String, isEnabled: Bool = true, lastSyncedAt: UInt64? = nil) {
        self.name = name
        self.sshDestination = sshDestination
        self.isEnabled = isEnabled
        self.lastSyncedAt = lastSyncedAt
    }

    /// Absolute path to this machine's mirror home
    /// (`~/Library/Application Support/TokenBar/RemoteMachines/<name>/home`).
    /// The fake home layout mirrors a real `~`: `.claude/`, `.codex/`,
    /// `.hermes/`, `.local/share/opencode/`, so the vendored scanner resolves
    /// every client root from `home` with env roots disabled.
    public var mirrorHomePath: String {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
            .appendingPathComponent("TokenBar/RemoteMachines/\(name)/home", isDirectory: true)
        return base.path
    }

    /// The per-client relative paths (from the remote home) that this product
    /// mirrors. Keys are the scanner's client roots; values are the source
    /// directories on the remote machine. Order matters for overlap: rsync
    /// runs each line and `--delete` is only applied to the claude root.
    public static let mirroredRoots: [(remotePath: String, mirrorRelative: String)] = [
        (".claude/projects", ".claude/projects"),
        (".codex/sessions", ".codex/sessions"),
        (".codex/archived_sessions", ".codex/archived_sessions"),
        (".hermes", ".hermes"),
        (".local/share/opencode", ".local/share/opencode"),
    ]
}

/// Combined view over the local machine and every enabled remote mirror.
///
/// The merge is a sum over already-pre-aggregated payloads at the machine
/// boundary — each machine's payload is fully aggregated in Rust, and Swift
/// only adds whole-machine buckets. It never subtracts a client from a mixed
/// bucket, so it cannot violate the pre-aggregation contract.
public struct RemoteUsage: Sendable {
    public let local: UsagePayload
    public let remote: [String: UsagePayload]

    public init(local: UsagePayload, remote: [String: UsagePayload]) {
        self.local = local
        self.remote = remote
    }

    /// Local payload plus every enabled remote mirror's payload.
    public var combined: UsagePayload { Self.merge(local: local, remotes: Array(remote.values)) }

    /// A single machine's payload: `nil` = local, otherwise the machine name.
    public func payload(for machine: String?) -> UsagePayload {
        guard let machine, let remotePayload = remote[machine] else { return local }
        return remotePayload
    }

    /// Merge several machine payloads by summing contribution buckets,
    /// model entries, years, and summary. Dates/models absent from one side
    /// are carried through unchanged.
    static func merge(local: UsagePayload, remotes: [UsagePayload]) -> UsagePayload {
        var all = remotes
        all.insert(local, at: 0)
        guard all.count > 1 else { return local }

        var contributionsByDate: [String: Contribution] = [:]
        for payload in all {
            for contribution in payload.contributions {
                var merged = contributionsByDate[contribution.date] ?? contribution
                if contributionsByDate[contribution.date] != nil {
                    merged = Self.addContribution(merged, contribution)
                }
                contributionsByDate[contribution.date] = merged
            }
        }
        let contributions = contributionsByDate.keys.sorted().compactMap { contributionsByDate[$0] }

        var yearsByKey: [String: YearMeta] = [:]
        for payload in all {
            for year in payload.years {
                var merged = yearsByKey[year.year] ?? year
                if yearsByKey[year.year] != nil {
                    merged = YearMeta(
                        year: year.year,
                        totalTokens: merged.totalTokens.saturatingAdding(year.totalTokens),
                        totalCost: merged.totalCost + year.totalCost,
                        range: DateRange(start: min(merged.range.start, year.range.start), end: max(merged.range.end, year.range.end))
                    )
                }
                yearsByKey[year.year] = merged
            }
        }
        let years = yearsByKey.keys.sorted().compactMap { yearsByKey[$0] }

        var totalTokens: Int64 = 0
        var totalCost: Double = 0
        var totalDays = 0
        var activeDays = 0
        var maxCostInSingleDay: Double = 0
        var clients = Set<String>()
        var models = Set<String>()
        var firstDate = contributions.first?.date ?? ""
        var lastDate = contributions.last?.date ?? ""
        for payload in all {
            totalTokens = totalTokens.saturatingAdding(payload.summary.totalTokens)
            totalCost += payload.summary.totalCost
            totalDays += payload.summary.totalDays
            activeDays += payload.summary.activeDays
            maxCostInSingleDay = max(maxCostInSingleDay, payload.summary.maxCostInSingleDay)
            clients.formUnion(payload.summary.clients)
            models.formUnion(payload.summary.models)
            if firstDate.isEmpty || payload.meta.dateRange.start < firstDate {
                firstDate = payload.meta.dateRange.start
            }
            if lastDate.isEmpty || payload.meta.dateRange.end > lastDate {
                lastDate = payload.meta.dateRange.end
            }
        }

        return UsagePayload(
            meta: .init(
                generatedAt: all.map(\.meta.generatedAt).max() ?? "",
                version: all.map(\.meta.version).max() ?? "",
                dateRange: DateRange(start: firstDate, end: lastDate)
            ),
            summary: .init(
                totalTokens: totalTokens,
                totalCost: totalCost,
                totalDays: totalDays,
                activeDays: activeDays,
                averagePerDay: totalDays > 0 ? Double(totalTokens) / Double(totalDays) : 0,
                maxCostInSingleDay: maxCostInSingleDay,
                clients: clients.sorted(),
                models: models.sorted()
            ),
            years: years,
            contributions: contributions
        )
    }

    private static func addContribution(_ a: Contribution, _ b: Contribution) -> Contribution {
        var clientsByKey: [String: ContributionClient] = [:]
        for client in a.clients + b.clients {
            let key = "\(client.client)|\(client.modelId)|\(client.providerId)"
            if var existing = clientsByKey[key] {
                existing = ContributionClient(
                    client: existing.client,
                    modelId: existing.modelId,
                    providerId: existing.providerId,
                    tokens: TokenBreakdown(
                        input: existing.tokens.input.saturatingAdding(client.tokens.input),
                        output: existing.tokens.output.saturatingAdding(client.tokens.output),
                        cacheRead: existing.tokens.cacheRead.saturatingAdding(client.tokens.cacheRead),
                        cacheWrite: existing.tokens.cacheWrite.saturatingAdding(client.tokens.cacheWrite),
                        reasoning: existing.tokens.reasoning.saturatingAdding(client.tokens.reasoning)
                    ),
                    cost: existing.cost + client.cost,
                    messages: existing.messages + client.messages
                )
                clientsByKey[key] = existing
            } else {
                clientsByKey[key] = client
            }
        }
        let clients = clientsByKey.values.sorted { $0.client < $1.client }
        let tokenBreakdown = TokenBreakdown(
            input: a.tokenBreakdown.input.saturatingAdding(b.tokenBreakdown.input),
            output: a.tokenBreakdown.output.saturatingAdding(b.tokenBreakdown.output),
            cacheRead: a.tokenBreakdown.cacheRead.saturatingAdding(b.tokenBreakdown.cacheRead),
            cacheWrite: a.tokenBreakdown.cacheWrite.saturatingAdding(b.tokenBreakdown.cacheWrite),
            reasoning: a.tokenBreakdown.reasoning.saturatingAdding(b.tokenBreakdown.reasoning)
        )
        return Contribution(
            date: a.date,
            totals: .init(
                tokens: a.totals.tokens.saturatingAdding(b.totals.tokens),
                cost: a.totals.cost + b.totals.cost,
                messages: a.totals.messages + b.totals.messages
            ),
            intensity: max(a.intensity, b.intensity),
            tokenBreakdown: tokenBreakdown,
            clients: clients
        )
    }

    /// Merge several model reports by summing per-(client, model, provider)
    /// entries. The entry key uses the same triple the Rust report groups by.
    public static func mergeModelReports(_ reports: [ModelReport]) -> ModelReport {
        guard reports.count > 1 else { return reports.first ?? .init(entries: [], totalInput: 0, totalOutput: 0, totalCacheRead: 0, totalCacheWrite: 0, totalMessages: 0, totalCost: 0, pricingUpdatedAt: nil) }
        var entriesByKey: [String: ModelReportEntry] = [:]
        var totalInput: Int64 = 0
        var totalOutput: Int64 = 0
        var totalCacheRead: Int64 = 0
        var totalCacheWrite: Int64 = 0
        var totalMessages = 0
        var totalCost: Double = 0
        var latestPricing: UInt64?
        for report in reports {
            totalInput = totalInput.saturatingAdding(report.totalInput)
            totalOutput = totalOutput.saturatingAdding(report.totalOutput)
            totalCacheRead = totalCacheRead.saturatingAdding(report.totalCacheRead)
            totalCacheWrite = totalCacheWrite.saturatingAdding(report.totalCacheWrite)
            totalMessages += report.totalMessages
            totalCost += report.totalCost
            if let updated = report.pricingUpdatedAt {
                latestPricing = max(latestPricing ?? 0, updated)
            }
            for entry in report.entries {
                let key = "\(entry.client)|\(entry.model)|\(entry.provider)"
                if var existing = entriesByKey[key] {
                    existing = ModelReportEntry(
                        client: existing.client,
                        model: existing.model,
                        provider: existing.provider,
                        input: existing.input.saturatingAdding(entry.input),
                        output: existing.output.saturatingAdding(entry.output),
                        cacheRead: existing.cacheRead.saturatingAdding(entry.cacheRead),
                        cacheWrite: existing.cacheWrite.saturatingAdding(entry.cacheWrite),
                        reasoning: existing.reasoning.saturatingAdding(entry.reasoning),
                        total: existing.total.saturatingAdding(entry.total),
                        messageCount: existing.messageCount + entry.messageCount,
                        cost: existing.cost + entry.cost,
                        msPer1kTokens: existing.msPer1kTokens
                    )
                    entriesByKey[key] = existing
                } else {
                    entriesByKey[key] = entry
                }
            }
        }
        return ModelReport(
            entries: entriesByKey.values.sorted { $0.model < $1.model },
            totalInput: totalInput,
            totalOutput: totalOutput,
            totalCacheRead: totalCacheRead,
            totalCacheWrite: totalCacheWrite,
            totalMessages: totalMessages,
            totalCost: totalCost,
            pricingUpdatedAt: latestPricing
        )
    }

    /// Merge several hourly reports by summing per-hour buckets.
    static func mergeHourlyReports(_ reports: [HourlyReport]) -> HourlyReport {
        guard reports.count > 1 else { return reports.first ?? .init(entries: [], totalCost: 0) }
        var entriesByHour: [String: HourlyReportEntry] = [:]
        var totalCost: Double = 0
        for report in reports {
            totalCost += report.totalCost
            for entry in report.entries {
                if var existing = entriesByHour[entry.hour] {
                    existing = HourlyReportEntry(
                        hour: existing.hour,
                        clients: Array(Set(existing.clients + entry.clients)).sorted(),
                        models: Array(Set(existing.models + entry.models)).sorted(),
                        input: existing.input.saturatingAdding(entry.input),
                        output: existing.output.saturatingAdding(entry.output),
                        cacheRead: existing.cacheRead.saturatingAdding(entry.cacheRead),
                        cacheWrite: existing.cacheWrite.saturatingAdding(entry.cacheWrite),
                        reasoning: existing.reasoning.saturatingAdding(entry.reasoning),
                        total: existing.total.saturatingAdding(entry.total),
                        messageCount: existing.messageCount + entry.messageCount,
                        turnCount: existing.turnCount + entry.turnCount,
                        cost: existing.cost + entry.cost
                    )
                    entriesByHour[entry.hour] = existing
                } else {
                    entriesByHour[entry.hour] = entry
                }
            }
        }
        return HourlyReport(
            entries: entriesByHour.keys.sorted().compactMap { entriesByHour[$0] },
            totalCost: totalCost
        )
    }

    /// Merge several agents reports by summing per-agent buckets.
    static func mergeAgentsReports(_ reports: [AgentsReport]) -> AgentsReport {
        guard reports.count > 1 else { return reports.first ?? .init(entries: [], totalCost: 0, totalMessages: 0) }
        var entriesByAgent: [String: AgentReportEntry] = [:]
        var totalCost: Double = 0
        var totalMessages = 0
        for report in reports {
            totalCost += report.totalCost
            totalMessages += report.totalMessages
            for entry in report.entries {
                if var existing = entriesByAgent[entry.agent] {
                    existing = AgentReportEntry(
                        agent: existing.agent,
                        clients: Array(Set(existing.clients + entry.clients)).sorted(),
                        input: existing.input.saturatingAdding(entry.input),
                        output: existing.output.saturatingAdding(entry.output),
                        cacheRead: existing.cacheRead.saturatingAdding(entry.cacheRead),
                        cacheWrite: existing.cacheWrite.saturatingAdding(entry.cacheWrite),
                        reasoning: existing.reasoning.saturatingAdding(entry.reasoning),
                        total: existing.total.saturatingAdding(entry.total),
                        cost: existing.cost + entry.cost,
                        messages: existing.messages + entry.messages
                    )
                    entriesByAgent[entry.agent] = existing
                } else {
                    entriesByAgent[entry.agent] = entry
                }
            }
        }
        return AgentsReport(
            entries: entriesByAgent.keys.sorted().compactMap { entriesByAgent[$0] },
            totalCost: totalCost,
            totalMessages: totalMessages
        )
    }
}
