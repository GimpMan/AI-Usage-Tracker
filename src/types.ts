export interface UsageWindow {
  label: string;
  used_percent: number;
  reset_at: string | null;
  /** Shown in the expanded popup but excluded from the collapsed status bar. */
  bar_visible: boolean;
  /** The provider explicitly reports that this window has no quota cap. */
  is_unlimited: boolean;
  /** Absolute usage counter when the provider reports one (e.g. Kimi). */
  used_absolute?: number | null;
  /** Absolute quota cap paired with `used_absolute`. */
  limit_absolute?: number | null;
}

export interface UsageSnapshot {
  provider: string;
  level: string | null;
  windows: UsageWindow[];
  unavailable_reason: string | null;
  fetched_at: string;
}

export type ProviderId =
  | "glm"
  | "minimax"
  | "codex"
  | "claude"
  | "grok"
  | "kimi"
  | "openrouter";

export type UpdatePhase =
  | "idle"
  | "checking"
  | "up_to_date"
  | "available"
  | "downloading"
  | "installing"
  | "error";

/** Exact serialized update channel values from the backend. */
export type UpdateChannel = "stable" | "prerelease";

/** Exact serialized shape of the Rust updater's shared state. */
export interface UpdateState {
  phase: UpdatePhase;
  current_version: string;
  available_version: string | null;
  notes: string | null;
  published_at: string | null;
  last_checked_at: string | null;
  downloaded_bytes: number;
  total_bytes: number | null;
  error: string | null;
  channel: UpdateChannel;
}

export interface ProviderConfig {
  id: ProviderId;
  label: string;
  auth_kind: "api_key" | "token_plan_key" | "local_files";
  has_region?: boolean;
}

/** Structured status returned by the backend `get_status` command. */
export interface ProviderStatus {
  id: ProviderId;
  label: string;
  needs_key: boolean;
  has_region: boolean;
  registered: boolean;
  configured: boolean;
  eligible: boolean;
  health:
    | "Healthy"
    | "MissingCredentials"
    | "InvalidCredentials"
    | "NoUsableDetails"
    | "TransientFailure";
  health_reason: string | null;
  hidden: boolean;
  region: string | null;
  unavailable_reason: string | null;
}

/** Burn history: one bar per bucket, relative-height bars + green reset marker. */
export interface BurnBucket { t: number; burn: number; reset: boolean; }
export interface WindowBurn { label: string; buckets: BurnBucket[]; }
export interface ProviderBurnHistory { id: string; provider: string; windows: WindowBurn[]; }
