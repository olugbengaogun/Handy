/**
 * Calendar periods for the Insights panel.
 *
 * The panel used to offer four rolling windows ending now, labelled "This
 * Week" and "This Month". Those labels were wrong: on the 3rd of a month, a
 * trailing 30 days is mostly the *previous* month. These are real calendar
 * periods, so a label and the number under it always agree, and they can be
 * stepped backwards.
 *
 * Every date here is a **local** calendar date. `toISOString()` is deliberately
 * never used: it converts to UTC, which shifts every boundary by a day west of
 * Greenwich and would silently mis-bucket a user's evening.
 */

export type PeriodKind = "day" | "week" | "month" | "year" | "all";

export interface Period {
  kind: PeriodKind;
  /** Any date inside the period; the bounds are derived from it. */
  anchor: Date;
}

/** Local `YYYY-MM-DD`. Never `toISOString`, which is UTC. */
export function toDayKey(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** Parse a local `YYYY-MM-DD` at local midnight. */
export function fromDayKey(key: string): Date {
  const [y, m, d] = key.split("-").map(Number);
  return new Date(y, m - 1, d);
}

export function startOfDay(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate());
}

/**
 * First day of the week for a locale: 1 = Monday … 7 = Sunday.
 *
 * Sunday in the US, Monday across most of Europe, so this cannot be hardcoded.
 * `getWeekInfo()` was an accessor named `weekInfo` before it became a method,
 * and support only began in Safari 17 — both shapes are probed, and the
 * fallback is Monday (the ISO-8601 default).
 */
export function firstDayOfWeek(locale: string): number {
  try {
    const loc = new Intl.Locale(locale) as Intl.Locale & {
      getWeekInfo?: () => { firstDay?: number };
      weekInfo?: { firstDay?: number };
    };
    const info =
      typeof loc.getWeekInfo === "function" ? loc.getWeekInfo() : loc.weekInfo;
    const first = info?.firstDay;
    if (typeof first === "number" && first >= 1 && first <= 7) return first;
  } catch {
    // Malformed locale tag, or an engine without the proposal. Fall through.
  }
  return 1;
}

/** Bounds of `period`, inclusive, as local dates. `all` has no fixed start. */
export function periodBounds(
  period: Period,
  locale: string,
): { start: Date | null; end: Date } {
  const a = startOfDay(period.anchor);

  switch (period.kind) {
    case "day":
      return { start: a, end: a };

    case "week": {
      // getDay() is 0=Sunday; weekInfo is 1=Monday..7=Sunday. Normalise both to
      // 0=Sunday before subtracting, or the offset is wrong for half the week.
      const firstDow = firstDayOfWeek(locale) % 7; // 7 (Sunday) -> 0
      const offset = (a.getDay() - firstDow + 7) % 7;
      const start = new Date(
        a.getFullYear(),
        a.getMonth(),
        a.getDate() - offset,
      );
      const end = new Date(
        start.getFullYear(),
        start.getMonth(),
        start.getDate() + 6,
      );
      return { start, end };
    }

    case "month": {
      const start = new Date(a.getFullYear(), a.getMonth(), 1);
      // Day 0 of the next month is the last day of this one — no table of
      // month lengths, and leap years take care of themselves.
      const end = new Date(a.getFullYear(), a.getMonth() + 1, 0);
      return { start, end };
    }

    case "year":
      return {
        start: new Date(a.getFullYear(), 0, 1),
        end: new Date(a.getFullYear(), 11, 31),
      };

    case "all":
      return { start: null, end: startOfDay(new Date()) };
  }
}

/**
 * Move a period by `delta` units.
 *
 * Month and year steps rebuild the date from parts rather than mutating with
 * `setMonth`, which on the 31st skips a month: 31 Jan + 1 month is 31 Feb,
 * which JavaScript normalises to 2 or 3 March. Anchoring to day 1 removes the
 * problem entirely, and the bounds are derived from the anchor anyway.
 */
export function shiftPeriod(period: Period, delta: number): Period {
  const a = period.anchor;
  switch (period.kind) {
    case "day":
      return {
        ...period,
        anchor: new Date(a.getFullYear(), a.getMonth(), a.getDate() + delta),
      };
    case "week":
      return {
        ...period,
        anchor: new Date(
          a.getFullYear(),
          a.getMonth(),
          a.getDate() + delta * 7,
        ),
      };
    case "month":
      return {
        ...period,
        anchor: new Date(a.getFullYear(), a.getMonth() + delta, 1),
      };
    case "year":
      return { ...period, anchor: new Date(a.getFullYear() + delta, 0, 1) };
    case "all":
      return period;
  }
}

/** Does this period contain today? Governs "current streak" and the › arrow. */
export function containsToday(period: Period, locale: string): boolean {
  const today = startOfDay(new Date());
  const { start, end } = periodBounds(period, locale);
  if (end < today) return false;
  return start === null || start <= today;
}

/**
 * Can we step back? False once the period already reaches past the first
 * recorded day, so the arrow dies at the edge of the record rather than
 * scrolling forever through empty months.
 */
export function canGoBack(
  period: Period,
  locale: string,
  firstRecorded: string | null,
): boolean {
  if (period.kind === "all" || !firstRecorded) return false;
  const { start } = periodBounds(period, locale);
  return start !== null && start > fromDayKey(firstRecorded);
}

/** Never navigate into the future: there is nothing there yet. */
export function canGoForward(period: Period, locale: string): boolean {
  return period.kind !== "all" && !containsToday(period, locale);
}

/**
 * The period's own name — "Today", "6–12 Aug", "August 2026", "2026".
 *
 * This *is* the label. A period that names itself cannot drift out of sync with
 * the figure beside it, which is exactly how "This Week" came to mean the last
 * seven days.
 */
export function formatPeriod(
  period: Period,
  locale: string,
  t: (key: string) => string,
): string {
  const { start, end } = periodBounds(period, locale);
  const today = startOfDay(new Date());

  switch (period.kind) {
    case "day":
      if (end.getTime() === today.getTime())
        return t("settings.insights.today");
      return end.toLocaleDateString(locale, {
        weekday: "short",
        day: "numeric",
        month: "short",
        year: end.getFullYear() === today.getFullYear() ? undefined : "numeric",
      });

    case "week": {
      const s = start as Date;
      const sameMonth = s.getMonth() === end.getMonth();
      const sameYear = s.getFullYear() === today.getFullYear();
      const left = s.toLocaleDateString(locale, {
        day: "numeric",
        ...(sameMonth ? {} : { month: "short" }),
      });
      const right = end.toLocaleDateString(locale, {
        day: "numeric",
        month: "short",
        ...(sameYear ? {} : { year: "numeric" }),
      });
      return `${left} – ${right}`;
    }

    case "month":
      return end.toLocaleDateString(locale, {
        month: "long",
        year: "numeric",
      });

    case "year":
      return String(end.getFullYear());

    case "all":
      return t("settings.insights.range.allTime");
  }
}
