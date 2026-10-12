/** Cooldown countdown math, decoupled from the DOM so it is unit-testable. */

export function cooldownRemainingMs(
  cooldownUntil: string | null,
  nowMs: number,
): number {
  if (cooldownUntil === null) return 0;
  const until = Date.parse(cooldownUntil);
  if (Number.isNaN(until)) return 0;
  return Math.max(0, until - nowMs);
}

/** "1:05" under an hour, "1:02:03" above. */
export function formatCountdown(ms: number): string {
  const totalSeconds = Math.ceil(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  if (hours > 0) return `${hours}:${pad(minutes)}:${pad(seconds)}`;
  return `${minutes}:${pad(seconds)}`;
}

export interface CountdownCallbacks {
  onTick: (remainingMs: number) => void;
  onExpire?: () => void;
}

/**
 * Live countdown for one cooldown_until value. Starts expired values at
 * zero and fires onExpire exactly once. Call stop() on teardown.
 */
export class Countdown {
  private intervalId: ReturnType<typeof setInterval> | null = null;
  private expiredNotified = false;

  constructor(
    private readonly cooldownUntil: string | null,
    private readonly callbacks: CountdownCallbacks,
    private readonly intervalMs = 500,
  ) {}

  start(): void {
    this.stop();
    this.expiredNotified = false;
    if (this.cooldownUntil === null) return;
    this.tick();
    this.intervalId = setInterval(() => this.tick(), this.intervalMs);
  }

  stop(): void {
    if (this.intervalId !== null) {
      clearInterval(this.intervalId);
      this.intervalId = null;
    }
  }

  private tick(): void {
    const remaining = cooldownRemainingMs(
      this.cooldownUntil,
      Date.now(),
    );
    this.callbacks.onTick(remaining);
    if (remaining === 0 && !this.expiredNotified) {
      this.expiredNotified = true;
      this.stop();
      this.callbacks.onExpire?.();
    }
  }
}
