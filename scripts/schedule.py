#!/usr/bin/env python3
"""Cost-balanced row-level schedule for the validation legs.

The channel legs (python / c / cli / core, and later the rust reference) used
to take whole objects, so one leg held the one row that costs 42 minutes per
run and every channel times it best of three (run 42506609035: the c and cli
legs holding 2020 CD3 hit the 240-minute limit and the run could not go
green). A leg now takes ROWS. This file decides which rows, and is a pure
function of (plan, cost table, leg budget): the same inputs give the same
bytes, so a schedule can be reproduced, diffed and tested.

    schedule.py pack --plan results/validation_plan.json \
        --costs fixtures/costs.json --channels python,c,cli,core \
        --budget 50 --out results/schedule.json --matrix results/leg-matrix.json
        writes the schedule and the leg matrix ([{"channel","leg"}], the only
        thing a job output carries) and prints each leg's predicted minutes.
        Every row the cost table does not know is priced at a class estimate
        and NAMED in the schedule and on stderr, never priced at zero.

    schedule.py validate --plan P --schedule S --costs C
        re-derives the units from the plan and refuses a schedule that drops
        or repeats a row, splits a unit, opens a c/cli leg on a heavy
        propagation row, or puts a row's timing replicas in one leg.

    schedule.py refresh-costs --from DIR --run-id ID --engine-pin PIN \
        --out fixtures/costs.json
        rebuilds the committed cost table from a named run's plan, rust
        reference and channel outputs. The table goes in through a PR.

    schedule.py pack-reference --candidates results/reference-candidates.json \
        --objects results/objects.json --costs fixtures/costs.json --legs 16 \
        --out results/reference-schedule.json --matrix results/reference-leg-matrix.json
        schedules the rust REFERENCE the same way: the candidate rows (what
        `validate run --list-rows` prints, in emission order) and one OD unit
        per object are packed into legs longest-first. A leg runs
        `validate run --select` for its rows and `validate od --only` for its
        objects (scripts/reference-legs.py).

    schedule.py validate-reference --candidates C --objects O --schedule S --costs T
        the validator's refusals for a reference schedule.

Heavy rows (S4). A row whose single timing run costs HEAVY_MIN_S or more is
fanned out: its timing repetitions run on that many distinct legs, one each,
and the merge (scripts/replicas.py) checks the results identical and keeps the
minimum time. The cost table marks such rows (`heavy.rows`; today only the
2020 CD3 propagation at dt = -5475 d, `auto`); `--heavy-min-s` lowers the
threshold so a cheap row fans out (the act rehearsal and the tests use it).
The fan-out count is the channel's own timed repetitions for the row
(reps()), so no replica leg runs a repetition the channel did not run before.
A leg's repetition overrides ride in the schedule (`reps`), and the side file
the drivers read is written by `select`; a plan key would flow into outputs.

Row identity: (object, test_type, propagation_uncertainty, dt for propagation
and ephemeris rows or 0, observer, force_model), unique across the plan.

Scheduling units, the smallest pieces a leg can take (a heavy row is one unit
that appears in each of its replica legs):

  * a propagation or ephemeris row: one unit, in every channel;
  * rust OD: one unit per object (the none / first_order rows reuse the legacy
    fit and both sidecars are written per object);
  * python / c / cli OD: the legacy untagged fit and the object's
    non_grav_recovery row are one unit (the legacy fit's second pass writes
    the NG row, and the row account pairs them); every other OD-family row is
    its own unit;
  * core: every row.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import statistics
import sys
from collections import defaultdict
from pathlib import Path

SCHEMA = 1
PROP_EPH = ("propagation", "ephemeris")
OD_FAMILY = (
    "orbit_determination",
    "orbit_determination_radar",
    "orbit_determination_transport",
    "non_grav_recovery",
)
CHANNELS = ("rust", "python", "c", "cli", "core")
# The c and cli daemons run five untimed warm-up runs on the first propagation
# row of each session: the first propagation row of a leg must be light, or the
# leg grows by five heavy runs (about 4 h on 2020 CD3).
WARMUP_CHANNELS = ("c", "cli")
LEADER_MAX_S = 10.0
# Rows costing at least this are stored by name in the cost table; lighter rows
# are priced at the class median of their (test_type, method).
EXPLICIT_MIN_S = 1.0
SAMPLING = {"sigma_point_detection_on", "monte_carlo_detection_on", "gaussian_mixture_detection_on"}
# The rust runner reuses the legacy fit for these OD tags (no extra solve).
RUST_OD_REUSE = {"none_detection_on", "first_order_detection_on"}
# Measured residual per cli row (chunks 02-10 of run 42506609035), seconds.
OVERHEAD_S = {"cli": 0.26}
# A row whose single timing run costs at least this many seconds is heavy: its
# timing repetitions fan out to distinct legs. The cost table marks the rows
# (refresh-costs writes `heavy`); this is the threshold it uses.
HEAVY_MIN_S = 600.0


# ── identity ────────────────────────────────────────────────────────────────


def row_key(r: dict) -> tuple:
    tt = r["test_type"]
    dt = float(r["dt_days"]) if tt in PROP_EPH else 0.0
    return (r["object"], tt, r.get("propagation_uncertainty"), dt, r.get("observer"), r.get("force_model"))


def key_str(k: tuple) -> str:
    return "|".join("" if v is None else repr(v) if isinstance(v, float) else str(v) for v in k)


def class_of(r: dict) -> str:
    return f"{r['test_type']}|{r.get('propagation_uncertainty') or ''}"


def group_of(r: dict) -> str:
    """The cost table's compact row group: everything in the key but the dt."""
    return "|".join(
        "" if v is None else str(v)
        for v in (r["object"], r["test_type"], r.get("propagation_uncertainty"), r.get("observer"), r.get("force_model"))
    )


def load_json(path: Path):
    with open(path) as f:
        return json.load(f)


def file_sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def reps(channel: str, r: dict) -> int:
    """Timed repetitions the channel runs per row (design, "The cost model")."""
    tt, m = r["test_type"], r.get("propagation_uncertainty")
    if tt == "propagation":
        if channel in ("c", "cli"):
            return 3
        return 1 if m in SAMPLING else 3
    if tt == "ephemeris":
        if channel == "core":
            return 1
        if channel in ("python", "c", "cli"):
            return 3
        return 1 if m in SAMPLING else 3  # rust
    return 1


def zero_cost(channel: str, r: dict) -> bool:
    """Rows the channel does not time: rust reuses the legacy fit; python, c and
    cli do not produce transport rows."""
    tt, m = r["test_type"], r.get("propagation_uncertainty")
    if channel == "rust" and tt == "orbit_determination" and m in RUST_OD_REUSE:
        return True
    return channel in ("python", "c", "cli") and tt == "orbit_determination_transport"


# ── units ───────────────────────────────────────────────────────────────────


def units(channel: str, plan: list) -> list[list[int]]:
    """Scheduling units as lists of plan indices, in order of first row."""
    if channel not in CHANNELS:
        raise SystemExit(f"ERROR: unknown channel {channel!r}; expected one of {', '.join(CHANNELS)}.")
    out: list[list[int]] = []
    od_unit: dict = {}
    for i, r in enumerate(plan):
        tt = r["test_type"]
        if tt in PROP_EPH or channel == "core":
            out.append([i])
            continue
        if tt not in OD_FAMILY:
            raise SystemExit(f"ERROR: plan row {i} has test_type {tt!r}, which no scheduling unit covers.")
        if channel == "rust":
            ukey = r["object"]
        elif tt == "non_grav_recovery" or (tt == "orbit_determination" and r.get("propagation_uncertainty") is None):
            ukey = (r["object"], "legacy+ng")
        else:
            ukey = (r["object"], i)  # its own unit
        if ukey not in od_unit:
            od_unit[ukey] = []
            out.append(od_unit[ukey])
        od_unit[ukey].append(i)
    return out


# ── cost table ──────────────────────────────────────────────────────────────


class CostTable:
    """The committed per-row cost table (fixtures/costs.json).

    channels.<ch>.rows         key -> minutes, rows costing >= 1 s
    channels.<ch>.class_median "test_type|method" -> minutes
    channels.<ch>.class_max    "test_type|method" -> minutes
    known_rows                 compact set of every row the run measured:
                               "object|test_type|method|observer|force_model" -> [dt, ...]
    """

    def __init__(self, data: dict, sha256: str):
        if data.get("schema") != SCHEMA:
            raise SystemExit(f"ERROR: cost table schema {data.get('schema')!r}, expected {SCHEMA}.")
        self.data = data
        self.sha256 = sha256
        self.known = {g: set(dts) for g, dts in data["known_rows"].items()}
        self.heavy_rows = set(data.get("heavy", {}).get("rows", []))

    @classmethod
    def load(cls, path: Path) -> CostTable:
        with open(path) as f:
            return cls(json.load(f), file_sha256(path))

    def is_known(self, r: dict) -> bool:
        dts = self.known.get(group_of(r))
        return dts is not None and (float(r["dt_days"]) if r["test_type"] in PROP_EPH else 0.0) in dts

    def price(self, channel: str, r: dict) -> tuple[float, str | None]:
        """Predicted minutes of one row, and the reason when it is an estimate
        (None for a row the table knows). The fallback is never zero: a row
        the table has not seen is priced at its class median (the class maximum
        for an OD-family row), and a class it has not seen at the channel's
        largest class median."""
        ch = self.data["channels"][channel]
        over = OVERHEAD_S.get(channel, 0.0) / 60.0
        k = key_str(row_key(r))
        if k in ch["rows"]:
            return ch["rows"][k] + over, None
        if self.is_known(r):
            return ch["class_median"][class_of(r)] + over, None
        cls = class_of(r)
        if cls in ch["class_max"]:
            tbl = ch["class_max"] if r["test_type"] in OD_FAMILY else ch["class_median"]
            return tbl[cls] + over, "row not in the cost table; priced at its class " + (
                "maximum" if r["test_type"] in OD_FAMILY else "median"
            )
        return max(ch["class_median"].values(), default=0.0) + over, "class not in the cost table; priced at the channel's largest class median"


def price_units(channel: str, plan: list, table: CostTable, unit_list: list[list[int]]):
    """Per-unit minutes, and the named list of estimated rows."""
    estimated: list[str] = []
    cache: dict[int, float] = {}
    costs: list[float] = []
    for u in unit_list:
        c = 0.0
        for i in u:
            r = plan[i]
            if zero_cost(channel, r):
                cache[i] = 0.0
                continue
            v, why = table.price(channel, r)
            cache[i] = v
            if why:
                estimated.append(f"{key_str(row_key(r))}: {why}")
            c += v
        costs.append(c)
    return costs, cache, estimated


# ── packing ─────────────────────────────────────────────────────────────────


def leg_counts(totals: dict[str, tuple[float, float]], budget: int) -> dict[str, int]:
    """Spread the leg budget over channels so their legs run about equally long.

    totals[channel] = (total minutes, heaviest unit minutes). The common target
    wall T is the smallest value for which sum(ceil(total / T)) fits the budget
    (binary search); leftover legs go one at a time to the channel with the
    largest total / legs. The whole budget is spent even when a heavy unit
    already sets the longest leg: the other legs get shorter, which is the
    margin against a slow runner. A channel never gets more legs than it has
    units (pack caps it). Deterministic: ties break by channel name."""
    chans = sorted(totals)
    if budget < len(chans):
        raise SystemExit(f"ERROR: the leg budget {budget} is below the channel count {len(chans)}.")

    def need(t: float) -> dict[str, int]:
        return {c: max(1, math.ceil(totals[c][0] / t)) for c in chans}

    lo, hi = 1e-3, max(v[0] for v in totals.values()) + 1.0
    for _ in range(80):
        mid = (lo + hi) / 2
        if sum(need(mid).values()) <= budget:
            hi = mid
        else:
            lo = mid
    n = need(hi)
    for _ in range(budget - sum(n.values())):
        worst = max(chans, key=lambda c: (totals[c][0] / n[c], c))
        n[worst] += 1
    return n


def heavy_units(channel: str, plan: list, table: CostTable, ul: list[list[int]], costs: list[float], heavy_min_s: float | None) -> dict[int, int]:
    """Units that fan out, as {unit index: replica count}. A heavy unit is a
    single propagation or ephemeris row the channel times more than once (so
    there are repetitions to spread) and that the cost table marks heavy, or,
    when `heavy_min_s` is given, whose single run costs at least that many
    seconds. The replica count is the channel's own repetition count."""
    out: dict[int, int] = {}
    for j, u in enumerate(ul):
        if len(u) != 1:
            continue
        r = plan[u[0]]
        if r["test_type"] not in PROP_EPH:
            continue
        n = reps(channel, r)
        if n < 2:
            continue
        marked = key_str(row_key(r)) in table.heavy_rows
        slow = heavy_min_s is not None and costs[j] * 60.0 / n >= heavy_min_s
        if marked or slow:
            out[j] = n
    return out


def pack(channel: str, plan: list, table: CostTable, n_legs: int, heavy_min_s: float | None = None):
    """Greedy longest-first packing. Ties break by plan index, so the result is
    a pure function of its inputs. For c and cli, each leg is first seeded with
    a light propagation row (the warm-up leader), and a heavy propagation unit
    only goes to a leg whose leader precedes it in plan order. A heavy row (see
    heavy_units) is placed once per replica, each replica on a distinct leg and
    priced at one run; its legs carry a repetition override of 1.

    Returns (legs, estimated, heaviest unit minutes, total minutes, replicas)."""
    ul = units(channel, plan)
    costs, rowcost, estimated = price_units(channel, plan, table, ul)
    if n_legs < 1:
        raise SystemExit(f"ERROR: {channel} needs at least one leg (got {n_legs}).")
    n_legs = min(n_legs, len(ul))
    heavy = heavy_units(channel, plan, table, ul, costs, heavy_min_s)
    max_rep = max(heavy.values(), default=1)
    if heavy and n_legs < max_rep:
        raise SystemExit(
            f"ERROR: {channel} has {n_legs} legs, but a heavy row fans out to {max_rep} distinct legs; "
            "the replicas of one row must not share a leg."
        )
    load = [0.0] * n_legs
    legs: list[list[int]] = [[] for _ in range(n_legs)]
    leader_idx = [None] * n_legs
    placed: set[int] = set()
    override: list[dict[int, int]] = [{} for _ in range(n_legs)]
    replica_legs: dict[int, list[int]] = {}

    def light_prop(j: int) -> bool:
        u = ul[j]
        return plan[u[0]]["test_type"] == "propagation" and costs[j] * 60.0 < LEADER_MAX_S

    if channel in WARMUP_CHANNELS:
        lead = [j for j in range(len(ul)) if light_prop(j) and j not in heavy][:n_legs]
        for k, j in enumerate(lead):
            legs[k].extend(ul[j])
            load[k] += costs[j]
            leader_idx[k] = ul[j][0]
            placed.add(j)
    # Work items: (unit, cost placed). A heavy unit becomes one item per
    # replica at the cost of one run.
    items: list[tuple[int, float]] = []
    for j in range(len(ul)):
        if j in placed:
            continue
        if j in heavy:
            items.extend((j, costs[j] / heavy[j]) for _ in range(heavy[j]))
        else:
            items.append((j, costs[j]))
    items.sort(key=lambda t: (-t[1], ul[t[0]][0]))
    for j, c in items:
        u = ul[j]
        heavy_prop = (
            channel in WARMUP_CHANNELS
            and plan[u[0]]["test_type"] == "propagation"
            and c * 60.0 >= LEADER_MAX_S
        )
        cand = [k for k in range(n_legs) if k not in replica_legs.get(j, [])]
        if heavy_prop:
            cand = [k for k in cand if leader_idx[k] is not None and leader_idx[k] < u[0]]
            if not cand:
                raise SystemExit(
                    f"ERROR: no {channel} leg can hold the heavy propagation row {key_str(row_key(plan[u[0]]))} "
                    "behind a light leader row."
                )
        k = min(cand, key=lambda k: (load[k], k))
        legs[k].extend(u)
        load[k] += c
        if j in heavy:
            replica_legs.setdefault(j, []).append(k)
            override[k][u[0]] = 1
    width = max(2, len(str(n_legs)))
    first_of = {u[0] for u in ul}
    out = []
    for k in range(n_legs):
        rows = sorted(legs[k])
        leg = {
            "leg": f"{k + 1:0{width}d}",
            "rows": rows,
            "units": sum(1 for i in rows if i in first_of),
            "predicted_min": round(load[k], 4),
        }
        if override[k]:
            leg["reps"] = {str(i): n for i, n in sorted(override[k].items())}
        out.append(leg)
    replicas = [
        {
            "channel": channel,
            "group": key_str(row_key(plan[ul[j][0]])),
            "row": ul[j][0],
            "legs": [out[k]["leg"] for k in sorted(ks)],
            "reps": 1,
        }
        for j, ks in sorted(replica_legs.items(), key=lambda t: ul[t[0]][0])
    ]
    return out, estimated, max((costs[j] for j in range(len(ul))), default=0.0), sum(costs), replicas


def build_schedule(
    plan_path: Path,
    costs_path: Path,
    channels: list[str],
    budget: int,
    legs: dict[str, int] | None,
    heavy_min_s: float | None = None,
):
    with open(plan_path) as f:
        plan = json.load(f)
    table = CostTable.load(costs_path)
    totals = {}
    fan_out = {}
    for c in channels:
        ul = units(c, plan)
        costs, _, _ = price_units(c, plan, table, ul)
        totals[c] = (sum(costs), max(costs, default=0.0))
        fan_out[c] = max(heavy_units(c, plan, table, ul, costs, heavy_min_s).values(), default=1)
    counts = dict(legs) if legs else leg_counts(totals, budget)
    if not legs:
        # A heavy row's replicas need that many distinct legs; a small budget
        # (a rehearsal) must not turn that into a refusal.
        for c in channels:
            counts[c] = max(counts[c], fan_out[c])
    sched = {
        "schema": SCHEMA,
        "plan_sha256": file_sha256(plan_path),
        "plan_rows": len(plan),
        "cost_table_sha256": table.sha256,
        "cost_table_run": table.data.get("run_id"),
        "leg_budget": budget,
        "heavy_min_s": heavy_min_s,
        "channels": {},
        "replicas": [],
    }
    for c in channels:
        legs_c, est, _, total, reps_c = pack(c, plan, table, counts[c], heavy_min_s)
        sched["channels"][c] = {"total_min": round(total, 3), "legs": legs_c, "estimated_rows": est}
        sched["replicas"].extend(reps_c)
    return sched, plan, table


def leg_matrix(sched: dict) -> list[dict]:
    return [{"channel": c, "leg": leg["leg"]} for c in sched["channels"] for leg in sched["channels"][c]["legs"]]


# ── validation ──────────────────────────────────────────────────────────────


def validate_channel(ch: str, body: dict, plan: list, table: CostTable, replicas: list[dict], bad: list[str]) -> None:
    """The partition, unit-closure, warm-up and replica refusals for one
    channel's legs, appended to `bad`. Shared by the channel and the reference
    schedules."""
    n = len(plan)
    reps_rows = {rep["row"]: rep for rep in replicas if rep["channel"] == ch}
    legs_of: dict[int, list[str]] = {}
    ids = [leg["leg"] for leg in body["legs"]]
    if len(set(ids)) != len(ids):
        bad.append(f"{ch}: duplicate leg ids")
    for leg in body["legs"]:
        if list(leg["rows"]) != sorted(set(leg["rows"])):
            bad.append(f"{ch} leg {leg['leg']}: rows are unsorted or repeated within the leg")
        for i in leg["rows"]:
            if not (0 <= i < n):
                bad.append(f"{ch} leg {leg['leg']}: row index {i} is outside the plan")
            else:
                legs_of.setdefault(i, []).append(leg["leg"])
    missing = [i for i in range(n) if i not in legs_of]
    if missing:
        bad.append(f"{ch}: {len(missing)} plan rows are in no leg (first: {missing[0]})")
    for i, ls in sorted(legs_of.items()):
        if i in reps_rows:
            continue
        if len(ls) > 1:
            bad.append(f"{ch}: plan row {i} is in legs {ls[0]} and {ls[1]}")
    for i, rep in sorted(reps_rows.items()):
        want = sorted(rep["legs"])
        have = sorted(legs_of.get(i, []))
        name = key_str(row_key(plan[i])) if 0 <= i < n else str(i)
        if want != have:
            bad.append(f"{ch}: replicas of {name} are declared in legs {want} but the row is in legs {have}")
        if len(set(want)) != len(want):
            bad.append(f"replicas of {rep['group']} ({ch}) share a leg")
        if 0 <= i < n and len(want) != reps(ch, plan[i]):
            bad.append(f"{ch}: {name} fans out to {len(want)} legs, but the channel times it {reps(ch, plan[i])} times")
        for leg in body["legs"]:
            if leg["leg"] in want and leg.get("reps", {}).get(str(i)) != rep.get("reps", 1):
                bad.append(f"{ch} leg {leg['leg']}: carries replica row {i} without its repetition override")
    for leg in body["legs"]:
        for i_str, cnt in leg.get("reps", {}).items():
            if int(i_str) not in reps_rows or leg["leg"] not in reps_rows[int(i_str)]["legs"]:
                bad.append(f"{ch} leg {leg['leg']}: repetition override on row {i_str}, which is not a declared replica of this leg")
    for u in units(ch, plan):
        legs_u = {tuple(legs_of.get(i, ())) for i in u}
        if len(legs_u) > 1 and not (len(u) == 1):
            bad.append(f"{ch}: unit {key_str(row_key(plan[u[0]]))} is split across legs {sorted(map(str, legs_u))}")
    if ch in WARMUP_CHANNELS:
        for leg in body["legs"]:
            first = next((i for i in leg["rows"] if plan[i]["test_type"] == "propagation"), None)
            if first is None:
                continue
            v, _ = table.price(ch, plan[first])
            if v * 60.0 >= LEADER_MAX_S:
                bad.append(
                    f"{ch} leg {leg['leg']}: opens on heavy propagation row {key_str(row_key(plan[first]))} "
                    f"({v * 60:.0f} s predicted); the daemon's warm-up would run on it"
                )


def validate(plan: list, plan_sha256: str, sched: dict, table: CostTable) -> list[str]:
    """Every refusal the schedule can earn, as readable lines (empty = valid)."""
    bad: list[str] = []
    if sched.get("schema") != SCHEMA:
        return [f"schedule schema {sched.get('schema')!r}, expected {SCHEMA}"]
    if sched["plan_sha256"] != plan_sha256:
        bad.append("the schedule was built for a different plan (hash mismatch)")
        return bad
    if sched["cost_table_sha256"] != table.sha256:
        bad.append("the schedule was built from a different cost table (hash mismatch)")
    replicas = sched.get("replicas", [])
    for ch, body in sched["channels"].items():
        validate_channel(ch, body, plan, table, replicas, bad)
    return bad


# ── reference (rust) schedule ───────────────────────────────────────────────


def od_pseudo_rows(objects: list[str], table: CostTable) -> list[dict]:
    """The OD-family rows each object's `validate od` run produces, taken from
    the cost table's record of the last measured run (known_rows), so the
    reference's OD units are priced exactly as the rust channel's OD rows are.
    An object the table has never seen gets one placeholder OD row, priced at
    the class maximum and named as an estimate."""
    by_obj: dict[str, list[dict]] = defaultdict(list)
    for g in sorted(table.known):
        obj, tt, method, observer, force = (g.split("|") + [""] * 5)[:5]
        if tt in OD_FAMILY:
            by_obj[obj].append(
                {
                    "object": obj,
                    "test_type": tt,
                    "propagation_uncertainty": method or None,
                    "dt_days": 0.0,
                    "observer": observer or None,
                    "force_model": force or None,
                }
            )
    out: list[dict] = []
    for o in objects:
        out.extend(by_obj.get(o) or [
            {
                "object": o,
                "test_type": "orbit_determination",
                "propagation_uncertainty": None,
                "dt_days": 0.0,
                "observer": None,
                "force_model": "standard",
            }
        ])
    return out


def build_reference_schedule(
    candidates_path: Path,
    objects_path: Path,
    costs_path: Path,
    n_legs: int,
    heavy_min_s: float | None = None,
):
    """Pack the reference: candidate rows plus one OD unit per object, into legs.

    The candidate list is what `validate run --list-rows` printed. The legs'
    `rows` index into it; `od_objects` name the objects whose `validate od`
    run the leg owns."""
    cands = load_json(candidates_path)
    objects = [o["name"] if isinstance(o, dict) else str(o) for o in load_json(objects_path)]
    table = CostTable.load(costs_path)
    od_rows = od_pseudo_rows(objects, table)
    full = cands + od_rows
    ul = units("rust", full)
    costs, _, _ = price_units("rust", full, table, ul)
    # A heavy row's replicas need that many distinct legs; a small leg count (a
    # rehearsal) must not turn that into a refusal.
    n_legs = max(n_legs, max(heavy_units("rust", full, table, ul, costs, heavy_min_s).values(), default=1))
    legs_all, est, _, total, reps_r = pack("rust", full, table, n_legs, heavy_min_s)
    n_c = len(cands)
    legs = []
    for leg in legs_all:
        rows = [i for i in leg["rows"] if i < n_c]
        od_objs = sorted({full[i]["object"] for i in leg["rows"] if i >= n_c}, key=objects.index)
        entry = {
            "leg": leg["leg"],
            "rows": rows,
            "od_objects": od_objs,
            "units": leg["units"],
            "predicted_min": leg["predicted_min"],
        }
        if "reps" in leg:
            entry["reps"] = leg["reps"]
        legs.append(entry)
    sched = {
        "schema": SCHEMA,
        "kind": "reference",
        "candidates_sha256": file_sha256(candidates_path),
        "candidate_rows": n_c,
        "objects": objects,
        "cost_table_sha256": table.sha256,
        "cost_table_run": table.data.get("run_id"),
        "leg_budget": n_legs,
        "heavy_min_s": heavy_min_s,
        "total_min": round(total, 3),
        "estimated_rows": est,
        "legs": legs,
        "replicas": reps_r,
    }
    return sched, cands, table


def validate_reference(cands: list, objects: list[str], cands_sha256: str, sched: dict, table: CostTable) -> list[str]:
    """The refusals for a reference schedule: every candidate row in a leg
    (replicas in each of theirs), every object's OD run in exactly one leg."""
    if sched.get("schema") != SCHEMA or sched.get("kind") != "reference":
        return [f"not a reference schedule of schema {SCHEMA}"]
    bad: list[str] = []
    if sched["candidates_sha256"] != cands_sha256:
        return ["the schedule was built for a different candidate list (hash mismatch)"]
    if sched["cost_table_sha256"] != table.sha256:
        bad.append("the schedule was built from a different cost table (hash mismatch)")
    body = {"legs": [{"leg": x["leg"], "rows": x["rows"], "reps": x.get("reps", {})} for x in sched["legs"]]}
    validate_channel("rust", body, cands, table, sched.get("replicas", []), bad)
    owner: dict[str, list[str]] = defaultdict(list)
    for leg in sched["legs"]:
        for o in leg["od_objects"]:
            owner[o].append(leg["leg"])
    for o in objects:
        if len(owner.get(o, [])) != 1:
            bad.append(f"the OD run of {o} is in legs {owner.get(o, [])}; it must be in exactly one")
    for o in owner:
        if o not in objects:
            bad.append(f"leg {owner[o][0]} runs the OD of {o}, which is not in the object list")
    return bad


# ── cost table refresh ──────────────────────────────────────────────────────


def _channel_rows(d: Path, ch: str) -> list[dict]:
    """A channel's rows from a run directory: the whole file, or its chunk / leg
    files anywhere below (symlinked directories followed)."""
    names = (f"validation_{ch}.json",)
    files = []
    for root, _dirs, fnames in os.walk(d, followlinks=True):
        for fn in fnames:
            if fn in names or (fn.startswith((f"validation_{ch}.chunk", f"validation_{ch}.leg")) and fn.endswith(".json")):
                files.append(Path(root) / fn)
    rows: list[dict] = []
    for f in sorted(files):
        with open(f) as fh:
            rows.extend(json.load(fh))
    return rows


def refresh_costs(src: Path, run_id: str, engine_pin: str) -> dict:
    """Rebuild the table from a run directory holding validation_plan.json, the
    rust reference (validation_rust.json) and each channel's output (whole, or
    per chunk / leg). A row a channel did not deliver (a leg that never
    finished) is priced from core's time scaled by the channel's measured
    channel/core ratio for the row's class, and the objects so priced are
    recorded."""
    plan = load_json(src / "validation_plan.json")
    emp: dict[str, dict] = {}
    for ch in CHANNELS:
        rows = load_json(src / "validation_rust.json") if ch == "rust" else _channel_rows(src, ch)
        emp[ch] = {key_str(row_key(r)): r.get("emp_time_ms") for r in rows}
    ratio: dict[str, dict] = {}
    for ch in ("python", "c", "cli"):
        agg: dict = defaultdict(lambda: [0.0, 0.0])
        for k, v in emp[ch].items():
            c = emp["core"].get(k)
            if v is None or c is None:
                continue
            kk = k.split("|")
            a = agg[(kk[1], kk[2])]
            a[0] += v
            a[1] += c
        ratio[ch] = {k: (x / y if y else 1.0) for k, (x, y) in agg.items()}
    out = {
        "schema": SCHEMA,
        "run_id": run_id,
        "engine_pin": engine_pin,
        "unit": "minutes of runner wall per row, repetitions included (emp_time_ms x timed repetitions)",
        "overhead_s_per_row": OVERHEAD_S,
        "explicit_min_s": EXPLICIT_MIN_S,
        "channels": {},
    }
    known: dict[str, set] = defaultdict(set)
    for r in plan:
        known[group_of(r)].add(float(r["dt_days"]) if r["test_type"] in PROP_EPH else 0.0)
    out["known_rows"] = {g: sorted(v) for g, v in sorted(known.items())}
    heavy: set[str] = set()
    for ch in CHANNELS:
        rows_c: dict[str, float] = {}
        by_class: dict[str, list[float]] = defaultdict(list)
        est_objects: set[str] = set()
        for r in plan:
            k = key_str(row_key(r))
            if zero_cost(ch, r):
                v = 0.0
            else:
                t = emp[ch].get(k)
                if t is None and ch != "core" and emp["core"].get(k) is not None:
                    t = emp["core"][k] * ratio.get(ch, {}).get((r["test_type"], r.get("propagation_uncertainty") or ""), 1.0)
                    est_objects.add(r["object"])
                v = (t or 0.0) * reps(ch, r) / 60000.0
            by_class[class_of(r)].append(v)
            if v * 60.0 >= EXPLICIT_MIN_S:
                rows_c[k] = round(v, 5)
            n_rep = reps(ch, r)
            if r["test_type"] in PROP_EPH and n_rep > 1 and v * 60.0 / n_rep >= HEAVY_MIN_S:
                heavy.add(k)
        out["channels"][ch] = {
            "rows": dict(sorted(rows_c.items())),
            "class_median": {c: round(statistics.median(v), 6) for c, v in sorted(by_class.items())},
            "class_max": {c: round(max(v), 5) for c, v in sorted(by_class.items())},
            "estimated_from_core_objects": sorted(est_objects),
        }
    out["heavy"] = {
        "min_single_run_s": HEAVY_MIN_S,
        "rows": sorted(heavy),
        "note": "rows whose single timing run costs at least min_single_run_s in some channel; their "
        "timing repetitions fan out to distinct legs (scripts/schedule.py, scripts/replicas.py)",
    }
    return out


# ── cli ─────────────────────────────────────────────────────────────────────


def summarize(sched: dict) -> str:
    lines = []
    for ch, body in sched["channels"].items():
        walls = [leg["predicted_min"] for leg in body["legs"]]
        lines.append(
            f"  {ch:6s} {len(walls):3d} legs  max {max(walls):7.1f} min  mean {sum(walls) / len(walls):6.1f}  "
            f"min {min(walls):6.1f}  total {body['total_min'] / 60:6.2f} h  estimated rows {len(body['estimated_rows'])}"
        )
    return "\n".join(lines)


def summarize_reference(sched: dict) -> str:
    walls = [leg["predicted_min"] for leg in sched["legs"]]
    return (
        f"  rust   {len(walls):3d} legs  max {max(walls):7.1f} min  mean {sum(walls) / len(walls):6.1f}  "
        f"min {min(walls):6.1f}  total {sched['total_min'] / 60:6.2f} h  estimated rows {len(sched['estimated_rows'])}  "
        f"replica rows {len(sched['replicas'])}"
    )


def write_json(obj, path: Path, indent=None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    try:
        with open(tmp, "w") as f:
            json.dump(obj, f, allow_nan=False, indent=indent, separators=(",", ":") if indent is None else None, sort_keys=True)
            f.write("\n")
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise
    tmp.replace(path)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("pack")
    p.add_argument("--plan", required=True, type=Path)
    p.add_argument("--costs", required=True, type=Path)
    p.add_argument("--channels", default="python,c,cli,core")
    p.add_argument("--budget", required=True, type=int, help="total legs over all channels")
    p.add_argument("--legs", default="", help="fixed leg counts, e.g. python=3,c=3 (overrides the budget split)")
    p.add_argument("--heavy-min-s", type=float, default=None, help="also fan out any row whose single run costs this many seconds")
    p.add_argument("--out", required=True, type=Path)
    p.add_argument("--matrix", type=Path)
    pr = sub.add_parser("pack-reference")
    pr.add_argument("--candidates", required=True, type=Path)
    pr.add_argument("--objects", required=True, type=Path)
    pr.add_argument("--costs", required=True, type=Path)
    pr.add_argument("--legs", required=True, type=int)
    pr.add_argument("--heavy-min-s", type=float, default=None)
    pr.add_argument("--out", required=True, type=Path)
    pr.add_argument("--matrix", type=Path)
    vr = sub.add_parser("validate-reference")
    vr.add_argument("--candidates", required=True, type=Path)
    vr.add_argument("--objects", required=True, type=Path)
    vr.add_argument("--schedule", required=True, type=Path)
    vr.add_argument("--costs", required=True, type=Path)
    v = sub.add_parser("validate")
    v.add_argument("--plan", required=True, type=Path)
    v.add_argument("--schedule", required=True, type=Path)
    v.add_argument("--costs", required=True, type=Path)
    r = sub.add_parser("refresh-costs")
    r.add_argument("--from", dest="src", required=True, type=Path)
    r.add_argument("--run-id", required=True)
    r.add_argument("--engine-pin", required=True)
    r.add_argument("--out", required=True, type=Path)
    args = ap.parse_args(argv)

    if args.cmd == "pack":
        channels = [c.strip() for c in args.channels.split(",") if c.strip()]
        legs = None
        if args.legs:
            legs = {kv.split("=")[0]: int(kv.split("=")[1]) for kv in args.legs.split(",")}
            missing = [c for c in channels if c not in legs]
            if missing:
                raise SystemExit(f"ERROR: --legs names no count for {', '.join(missing)}.")
        sched, plan, table = build_schedule(args.plan, args.costs, channels, args.budget, legs, args.heavy_min_s)
        bad = validate(plan, sched["plan_sha256"], sched, table)
        if bad:
            raise SystemExit("ERROR: the schedule failed its own validation:\n  " + "\n  ".join(bad))
        write_json(sched, args.out)
        if args.matrix:
            write_json(leg_matrix(sched), args.matrix)
        print(f"Schedule: {len(plan)} plan rows, cost table from run {table.data.get('run_id')} -> {args.out}")
        print(summarize(sched))
        for ch, body in sched["channels"].items():
            if body["estimated_rows"]:
                print(f"  ESTIMATED ({ch}): {len(body['estimated_rows'])} rows not in the cost table, priced at class estimates:", file=sys.stderr)
                for line in body["estimated_rows"][:50]:
                    print("    " + line, file=sys.stderr)
        return 0
    if args.cmd == "pack-reference":
        sched, cands, table = build_reference_schedule(args.candidates, args.objects, args.costs, args.legs, args.heavy_min_s)
        objects = sched["objects"]
        bad = validate_reference(cands, objects, sched["candidates_sha256"], sched, table)
        if bad:
            raise SystemExit("ERROR: the reference schedule failed its own validation:\n  " + "\n  ".join(bad))
        write_json(sched, args.out)
        if args.matrix:
            write_json([{"leg": leg["leg"]} for leg in sched["legs"]], args.matrix)
        print(f"Reference schedule: {len(cands)} candidate rows, {len(objects)} objects, cost table from run {table.data.get('run_id')} -> {args.out}")
        print(summarize_reference(sched))
        if sched["estimated_rows"]:
            print(f"  ESTIMATED (reference): {len(sched['estimated_rows'])} rows not in the cost table, priced at class estimates:", file=sys.stderr)
            for line in sched["estimated_rows"][:50]:
                print("    " + line, file=sys.stderr)
        return 0
    if args.cmd == "validate-reference":
        cands = load_json(args.candidates)
        sched = load_json(args.schedule)
        objects = [o["name"] if isinstance(o, dict) else str(o) for o in load_json(args.objects)]
        bad = validate_reference(cands, objects, file_sha256(args.candidates), sched, CostTable.load(args.costs))
        if bad:
            raise SystemExit("ERROR: invalid reference schedule:\n  " + "\n  ".join(bad))
        print(f"{args.schedule}: valid")
        return 0
    if args.cmd == "validate":
        plan = load_json(args.plan)
        bad = validate(plan, file_sha256(args.plan), load_json(args.schedule), CostTable.load(args.costs))
        if bad:
            raise SystemExit("ERROR: invalid schedule:\n  " + "\n  ".join(bad))
        print(f"{args.schedule}: valid")
        return 0
    table = refresh_costs(args.src, args.run_id, args.engine_pin)
    write_json(table, args.out, indent=1)
    print(f"{args.out}: cost table from run {args.run_id}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
