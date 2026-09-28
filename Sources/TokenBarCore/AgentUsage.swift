import Foundation

// OAuth/subscription quota cards (`AgentUsagePayload` in the Tauri
// frontend's src/lib/agentUsage.ts).

public struct AgentIdentity: Decodable, Sendable {
    public let email: String?
    public let plan: String?
}

public struct UsageWindow: Decodable, Sendable {
    public let label: String
    public let usedPercent: Double
    public let remainingPercent: Double
    public let resetsAt: String?
    public let resetText: String?
    /// Total window length in minutes; enables pace (expected vs actual).
    public let windowMinutes: Int64?
    /// Expected used-percent now from *historical* weekly samples (Codex weekly
    /// only, once enough past weeks accrued). Absent → fall back to linear pace.
    public let historicalExpectedPercent: Double?
    /// 0..1 chance the window empties before reset at the historical burn rate.
    public let runOutProbability: Double?

    // Memberwise init so --selftest can build fixture windows.
    public init(
        label: String, usedPercent: Double, remainingPercent: Double,
        resetsAt: String? = nil, resetText: String? = nil,
        windowMinutes: Int64? = nil, historicalExpectedPercent: Double? = nil,
        runOutProbability: Double? = nil
    ) {
        self.label = label
        self.usedPercent = usedPercent
        self.remainingPercent = remainingPercent
        self.resetsAt = resetsAt
        self.resetText = resetText
        self.windowMinutes = windowMinutes
        self.historicalExpectedPercent = historicalExpectedPercent
        self.runOutProbability = runOutProbability
    }
}

public struct CreditsSnapshot: Decodable, Sendable {
    public let remaining: Double?
    public let unlimited: Bool
}

public struct AgentUsageSnapshot: Decodable, Sendable {
    public let clientId: String
    public let source: String
    public let updatedAt: String
    public let identity: AgentIdentity?
    public let windows: [UsageWindow]
    public let credits: CreditsSnapshot?
    public let error: String?
}

public struct AgentUsagePayload: Decodable, Sendable {
    public let generatedAt: String
    public let agents: [AgentUsageSnapshot]
    /// Subscription-type providers opencode is authed against (e.g. ["Codex"]).
    /// Omitted from the JSON entirely when empty.
    public let opencodeSubscriptions: [String]?
}
