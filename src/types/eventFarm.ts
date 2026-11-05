/**
 * Rust `EventFarmConfig` (serde camelCase). Sent inside the run config
 * when the event-farm start command injects `eventFarm`.
 */
export interface EventFarmConfig {
  /** Quest name matched against the scanned card names; null farms the
   * highest-level unlocked farm quest. */
  targetQuest: string | null;
}
