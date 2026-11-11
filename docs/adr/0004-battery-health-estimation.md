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
3. **Metric.** Compare by capacity when at least two discharges have a power
   value and they make up at least half of all usable discharges. That evens
   out different loads, to first order. Otherwise compare full runtimes,
   which assumes a similar load. The confidence is lowered in that case.
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
   2×.
6. **Verdict.**
   - Replace: the UPS sets RB, or health is below 60 %.
   - Aging: health is below 80 %.
   - Good: otherwise.
   - Unknown: fewer than two usable discharges.

   Supporting reasons are also given:
   - recent outages lasting less than 70 % of the UPS's own runtime estimate;
   - battery age over 3 or 4 years, when the installation date is known.
7. **New battery.** The user enters the installation date, or clicks
   "Replaced today". Only outages from that date on are used, so a new
   battery gets a fresh baseline.
8. **Guided runtime test.**
   - The user arms the test (on mains, at least 95 % charge, stop level
     20–90 %, default 50 %) and unplugs the UPS.
   - The collector tags the resulting outage as a runtime test and raises a
     "plug it back in" notification at the stop level, or earlier if the UPS
     reports low battery. The notification repeats every 2 minutes.
   - An armed test expires after 30 minutes.
   - Nothing is sent to the UPS: the test is a normal, deliberate outage.

## Consequences
- The estimate measures the decline since monitoring began, not the
  battery's absolute state. A battery that was already worn when UPS-BMC was
  installed looks "100 %" at first. The UPS's RB flag, the runtime vs.
  UPS-estimate check and the battery age partly cover that case.
- Linear extrapolation overstates the runtime of shallow discharges, because
  lead-acid voltage, and with it the UPS's charge estimate, falls faster near
  the end. Comparing like with like (the baseline is made the same way) keeps
  the ratio meaningful. Runtime tests to 50 % are the most comparable data.
- With an estimated load, capacity in Wh is only as good as the estimate.
  Ratios stay valid as long as the estimate (and the real load) don't change.
  The UI marks such figures with "≈".
- Outage records gain a `runtime_test` flag (database migration v4). Reports
  label test outages. They still count as time on battery in availability
  statistics, which describe what the protected equipment saw.
- Future (M4+): use a UPS-initiated battery test where the device supports it
  (an ADR-0003 write, opt-in), and per-sample voltage curves for a
  voltage-sag metric (already recorded per outage as start/min voltage).
