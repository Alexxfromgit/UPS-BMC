# ADR-0004: Battery health estimation

- Status: accepted (M3)
- Date: 2026-11-11

## Context
The Eaton 5E reports battery charge (%), a runtime estimate, battery voltage
and a "replace battery" (RB) flag. It does not report capacity or internal
resistance, and it doesn't start its own battery test unless told to. UPS-BMC
stays read-only for that: the beeper is the only write (ADR-0003).

The load in this installation is small (a router, about 15–25 W). The UPS
reports that as 0 % / 0 W because it is below its measuring range (it
measures in ~1 % / 9 VA steps). The user can enter an estimated load
(Settings), which outages then record, marked as an estimate.

Lead-acid UPS batteries usually last 3–5 years. They lose capacity gradually
and then fail quickly near the end of their life.

## Decision
Estimate health **relative to the battery's own first discharges**, not as
an absolute capacity:

1. **Usable discharges.** An outage counts when its start and end were both
   observed, it started at 80 % charge or more, used at least 5 percentage
   points and lasted at least 60 s. Shorter or partial outages are recorded
   but not used: integer charge readings make small drops too coarse.
2. **Per discharge**, extrapolate linearly to a full-battery runtime:
   `duration × 100 / charge used`. When the average power is known (measured,
   or the user's estimate), also compute the capacity
   `power × full runtime` in Wh.
3. **Metric.** Compare by capacity, which evens out different loads to first
   order, only when all of these hold:
   - the discharges that form the baseline all have a power value, so the
     baseline stays the battery's oldest discharges and doesn't move when a
     load value appears later;
   - at least one power value was *measured*. If every value is the user's
     estimate, capacity = estimate × runtime adds nothing, and a refined
     estimate would fake a change in health;
   - at least two discharges, and at least half of them, have a power value.

   Otherwise compare full runtimes, which assumes a similar load, and lower
   the confidence. Discharges left out of a capacity comparison (no load) are
   marked "not compared".
4. **Baseline and recent.** The baseline is the median of the first
   `min(3, n/2)` discharges. The recent value is the median of the last
   `min(3, n − baseline)`. Health is recent ÷ baseline, capped at 100 %.
   Medians keep a single odd outage from swinging the result.
5. **Confidence.**
   - High: at least 6 discharges.
   - Medium: at least 4.
   - Low: fewer.

   It drops one level when the baseline or the recent group has no "strong"
   discharge (≥ 20 % used, or a runtime test). It also drops one level when
   runtimes are compared without load, or when the loads differ by more than
   2×. It drops one more level when the median depth of the two groups (charge
   used) differs by more than 2×: shallow discharges overstate the
   extrapolated runtime, so such a comparison is biased.
6. **Verdict.**
   - Replace: the UPS sets RB, or health is below 60 %.
   - Aging: health is below 80 %.
   - Good: otherwise.
   - Unknown: fewer than two usable discharges.

   Supporting reasons are also given:
   - the extrapolated runtime at the recent load being less than 70 % of the
     UPS's own runtime estimate;
   - battery age over 3 or 4 years, when the installation date is known.

   The UPS's RB flag is only used while the UPS is connected (a stale reading
   could keep or miss the verdict).
7. **New battery.** The user enters the installation date, or clicks
   "Replaced today". Only outages from then on are used, so a new battery gets
   a fresh baseline. A date counts from local midnight; "Replaced today"
   records the exact time, so the old battery's outages earlier that day are
   excluded.
8. **Guided runtime test.**
   - The user arms the test (on mains, at least 95 % charge, stop level
     20–90 %, default 50 %) and unplugs the UPS.
   - The collector tags the resulting outage as a runtime test and raises a
     "plug it back in" notification at the stop level, or earlier if the UPS
     reports low battery. The notification repeats every 2 minutes.
   - An armed test expires after 30 minutes.
   - Mains returning within a minute is a blip: the test goes back to armed
     and that short outage isn't tagged.
   - A test cancelled while discharging removes the tag, so the outage counts
     like any other.
   - The test state is saved in the database, so a collector restart during a
     test continues it (and reminds at once if the stop level has passed).
   - Nothing is sent to the UPS: the test is a normal, deliberate outage.

## Consequences
- The estimate measures the decline since monitoring began, not the
  battery's absolute state. A battery that was already worn when UPS-BMC was
  installed looks "100 %" at first. The UPS's RB flag, the runtime vs.
  UPS-estimate check and the battery age partly cover that case.
- Linear extrapolation overstates the runtime of shallow discharges, because
  lead-acid voltage, and with it the UPS's charge estimate, falls faster near
  the end. The ratio is only meaningful when the baseline and recent
  discharges have a similar depth; when they don't, the confidence is lowered
  and the reasons say so. Runtime tests to the same stop level are the most
  comparable data.
- With an estimated load, capacity in Wh is only as good as the estimate.
  Ratios stay valid as long as the estimate (and the real load) don't change.
  The UI marks such figures with "≈".
- Outage records gain a `runtime_test` flag (database migration v4). Reports
  label test outages.
- Changed after the independent review of 2026-11-12 (findings M3-1 to
  M3-12): the metric choice, the depth check, the stale RB flag, the local
  installation date and exact replacement time, the blip and cancel handling
  and the saved test state. They still count as time on battery in availability
  statistics, which describe what the protected equipment saw.
- Future (M4+): use a UPS-initiated battery test where the device supports it
  (an ADR-0003 write, opt-in), and per-sample voltage curves for a
  voltage-sag metric (already recorded per outage as start/min voltage).
