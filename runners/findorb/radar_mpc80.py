"""Convert an ADES radar table into MPC 80-column radar records for find_orb.

WHY THIS EXISTS
---------------
The fixtures in ``fixtures/psv-radar/`` carry TWO PSV tables in one file: the
normal optical ADES table, then a second header plus a radar table. Feeding
that file to find_orb as-is kills it:

    fo: ades2mpc.cpp:1252: check_for_psv_header:
        Assertion `cptr->psv_tags[n_psv_tags] > 0' failed.

Two independent problems produce that, and neither is fixable in the ADES path:

  1. ``sanitize_psv()`` takes the pos1/pos2/pos3 column INDICES from the
     optical header (7, 8, 9) and applies them to every subsequent line —
     including the radar header, where columns 7/8/9 are ``rmsDelay|com|frq``.
     It rewrites the radar header to ``...|rmsDelay |com.0| frq.0``,
     ``find_tag("com.0")`` returns -1, and the assert fires. That is our bug,
     and it is fixed by scoping the sanitizer to the optical table — which is
     what ``convert_radar_psv`` below exists to make possible. Reproduced on
     Apophis (51 radar-block lines mutated), Didymos (10) and Eros (7).

  2. Even with the sanitizer scoped correctly, find_orb's ADES *radar* reader
     still dies, in ``_format_without_decimal`` (ades2mpc.cpp:616,
     ``assert(n_before_decimal <= max_before_decimal)``): it does string
     surgery on the ADES text and cannot cope with our delay values that carry
     14-15 decimal places (float-repr artifacts of JPL's microsecond-to-second
     conversion, e.g. ``202.37852003999998``) nor with values written without a
     decimal point at all (``frq=8560``, ``rmsDelay=1``), where it dereferences
     a NULL decimal pointer. Bennu and Toutatis have no pos1/pos2/pos3 columns,
     so ``sanitize_psv`` never touches them — and they still SIGABRT here, which
     is what proves problem 2 is independent of problem 1.

So we bypass find_orb's ADES radar reader entirely and hand it radar in the
format it has read reliably for decades: the classic MPC 80-column radar
record pair. find_orb detects those on column 15 (``mpc_obs.cpp:1717``:
``buff[14] == 'R' || buff[14] == 'r'``) and they can be appended directly to
the ADES PSV file — find_orb's reader passes non-PSV lines through to the
80-column parser.

THE UNIT CONTRACT
-----------------
This is the highest-risk part of the conversion, because a wrong factor here
produces a fit that still CONVERGES and is quietly wrong. Every factor below
is cited to the IAU ADES normative definition and to find_orb's own parser,
and was checked numerically end-to-end (radar residuals land at 0.1-200
microseconds on 15-270 second delays; a factor-of-2 or factor-of-1e6 error
would show up 4-8 orders of magnitude away from that).

ADES side — IAU-ADES/ADES-Master ``xml/adesmaster.xml``, the normative file
that generates the schema and the readers, lines 1610-1613 / 1628-1629:

    delay      "Observed radar delay value in seconds."          -> SECONDS
    rmsDelay   "... uncertainty in \\si{\\micro\\second} ..."      -> MICROSECONDS
    doppler    "observed radar doppler value in \\si{\\hertz}"     -> Hz
    rmsDoppler "... uncertainty in \\si{\\hertz} ..."              -> Hz
    frq        "Carrier reference frequence in \\si{\\mega\\hertz}" -> MHz

The delay/rmsDelay unit mismatch (seconds vs microseconds in the same row) is
DELIBERATE in the ADES spec, not a fixture defect. Do not "fix" it.

find_orb side — ``mpc_obs.cpp:5121 extract_radar_value()`` reads a 15-char
field, blank-fills only field positions 11..14, runs C ``atof``, and multiplies
by 1e-4 (an implied decimal point 4 digits from the right). Its callers in
``compute_radar_info`` (mpc_obs.cpp:5186-5192) then apply:

    rtt_obs       = extract(first_line  + 32) * 1e-6   # field is MICROSECONDS
    rtt_sigma     = extract(second_line + 32) * 1e-6   # field is MICROSECONDS
    doppler_obs   = extract(first_line  + 47)          # field is Hz
    doppler_sigma = extract(second_line + 47)          # field is Hz

Composing the two gives the integer that must appear in each field:

    delay      round(delay_seconds       * 1e10)   # 1e-4 implied * 1e-6 s/us
    rmsDelay   round(rms_microseconds    * 1e4)    # 1e-4 implied, already us
    doppler    round(abs(doppler_hz)     * 1e4)    # 1e-4 implied, sign broken
                                                   #   out into its own byte
    rmsDoppler round(rms_doppler_hz      * 1e4)    # 1e-4 implied
    frq        round(frq_mhz             * 10)     # 5 int digits + tenths

THE ENCODING MUST BE NUMERIC, NOT STRING SURGERY
------------------------------------------------
The IAU ADES reference encoder (ADES-Master ``xmltompc80col.py``) builds these
fields by decimal-aligning the ADES *string*, which leaves EMBEDDED SPACES when
the value carries fewer than the full complement of decimals. find_orb blank-
fills only field positions 11..14 and then calls C ``atof``, which STOPS at the
first space. A doppler of ``-39889`` encoded that way parses ~1e4 too small,
and a delay of ``113.79639386`` s written as ``  113796`` + blanks parses as
11.38 MICROSECONDS instead of 113.8 seconds — and find_orb still exits 0 and
still "converges". That is exactly the failure mode this module exists to
prevent, so every field is rendered as a fully zero-padded integer.

NO HIDDEN FALLBACKS
-------------------
Every guard below raises :class:`RadarConversionError` naming the object and
the epoch. A radar row is never silently dropped, never emitted with a guessed
sigma, and never emitted with a value find_orb would misread.

Two of find_orb's own failure modes are silent and must be caught by the
CALLER, not here, because they only show up after fo has run:

  * a first line whose ``r`` mate does not match is dropped with the stdout
    line ``N 'line 1' radar observations didn't have a 'line 2' match.`` and
    EXIT CODE 0;
  * two radar records at the same JD are merged by ``fix_radar_obs`` with no
    message at all.

Both shrink the observation count without failing, so the caller must compare
the number of records emitted here against the number of ``note2`` R/r rows in
fo's residual listing and treat a shortfall as a failure.

References:
    IAU ADES:  https://github.com/IAU-ADES/ADES-Master  (xml/adesmaster.xml)
    MPC radar: https://www.minorplanetcenter.net/iau/info/RadarObs.html
    find_orb:  mpc_obs.cpp (parse_observation, extract_radar_value,
               compute_radar_info, fix_radar_obs)
"""

import re
from datetime import datetime, timezone
from decimal import ROUND_HALF_UP, Decimal, InvalidOperation
from typing import Dict, List, Optional, Tuple

# A PSV column name: a bare identifier. Used together with the presence of an
# ``obsTime`` column to tell a header line from a data line (see
# split_psv_tables).
_IDENTIFIER = re.compile(r"^[A-Za-z][A-Za-z0-9_]*$")

# find_orb's "mutant hex" digit alphabet, used both for the packed-designation
# century/cycle character and for the packed permID high digit.
# (find_orb mpc_fmt.cpp: int_to_mutant_hex_char)
MUTANT_HEX = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"

# find_orb treats two observations from the same station whose times differ by
# less than this as duplicates and silently collapses them
# (mpc_obs.cpp:2936 times_very_close -> sort_obs_by_date_and_remove_duplicates).
DUPLICATE_THRESHOLD_S = 3.0

# Field widths, in characters, of the four implied-decimal radar values.
# These are find_orb's, read off extract_radar_value's callers; they are NOT
# negotiable and an overflow must be a hard failure, because a too-wide value
# would silently shift into the neighbouring field.
W_DELAY = 15       # first_line[32:47]
W_DOPPLER = 14     # first_line[48:62]  (sign lives at [47])
W_RMS_DELAY = 14   # second_line[33:47] (the S/C flag lives at [32])
W_RMS_DOPPLER = 15  # second_line[47:62]
W_FRQ = 6          # first_line[62:68]  (5 integer digits + tenths)


class RadarConversionError(Exception):
    """A radar row could not be converted to a form find_orb reads correctly.

    Raised rather than skipping the row: a validation comparator that quietly
    drops observations reports a comparison it did not actually make.
    """


# ── PSV splitting ───────────────────────────────────────


def split_psv_tables(psv: str) -> List[Tuple[List[str], List[str], str]]:
    """Split a multi-table ADES PSV file into ``(fields, data_lines, raw_header)``.

    The psv-radar fixtures concatenate two PSV tables in one file with NO blank
    line and no marker between them — the optical table, then a second
    ``permID|trx|rcv|...`` header and the radar rows (Apophis line 9523, Bennu
    606, Didymos 6037, Eros 13153, Toutatis 7158).

    A header is recognised by two conditions together: one of its fields is the
    literal string ``obsTime``, and every field is a non-empty bare identifier.
    Both are needed. Keying on the first field being ``permID`` is NOT enough —
    the optical fixtures for unnumbered objects (1I/'Oumuamua, 2001 QR322, ...)
    start their header at ``provID``. And keying on the identifier shape alone
    is not enough either, because ADES trkSub values look like identifiers
    (``J99R36Q``). Every ADES table has an ``obsTime`` column, and no data row
    can carry the literal text ``obsTime`` as its timestamp, so the pair is
    unambiguous.

    The raw header line is carried alongside the stripped field names so the
    optical table can be re-emitted BYTE-IDENTICALLY to the fixture. The column
    padding is semantically irrelevant to PSV, but keeping it means the optical
    half of the radar pass is exactly the text the existing optical pass feeds
    find_orb, so any difference in the two fits is attributable to the radar.

    ``# version=...`` comment lines are dropped; the caller re-emits its own.
    """
    tables: List[Tuple[List[str], List[str], str]] = []
    for raw in psv.splitlines():
        if raw.startswith("#") or not raw.strip():
            continue
        fields = [f.strip() for f in raw.split("|")]
        if "obsTime" in fields and all(_IDENTIFIER.match(f) for f in fields):
            tables.append((fields, [], raw))
            continue
        if not tables:
            raise RadarConversionError(
                f"PSV data line before any header: {raw[:60]!r}"
            )
        tables[-1][1].append(raw)
    return tables


def find_radar_table(
    tables: List[Tuple[List[str], List[str], str]],
) -> Optional[int]:
    """Index of the radar table, or None if the file carries no radar.

    Identified by its columns, not by position: the radar header is NOT fixed
    across the fixtures. Didymos's radar table has no ``doppler`` or
    ``rmsDoppler`` columns AT ALL (they are absent, not blank), so anything
    that indexes fixed columns or insists ``doppler`` exists breaks on it.
    """
    hits = [
        i for i, (hdr, _, _) in enumerate(tables)
        if "delay" in hdr or "doppler" in hdr
    ]
    if not hits:
        return None
    if len(hits) > 1:
        raise RadarConversionError(
            f"expected at most one radar table, found {len(hits)}"
        )
    return hits[0]


# ── Field encoders ──────────────────────────────────────


def _decimal(obj: str, epoch: str, field: str, text: str) -> Decimal:
    try:
        return Decimal(text)
    except InvalidOperation:
        raise RadarConversionError(
            f"{obj} {epoch}: {field}={text!r} is not a number"
        ) from None


def _scaled_digits(
    obj: str, epoch: str, field: str, value: Decimal, scale: int, width: int
) -> str:
    """``|value| * 10**scale`` as exactly ``width`` zero-padded decimal digits.

    Zero-padded rather than space-padded, and rendered in full rather than
    truncated, for the atof reason in this module's docstring: any space inside
    the field (leading OR trailing, before position 11) truncates the value
    silently. Overflow is fatal — a value one digit too wide would run into the
    neighbouring field and be misread as something else entirely.
    """
    digits = (value.copy_abs() * (Decimal(10) ** scale)).quantize(
        Decimal(1), rounding=ROUND_HALF_UP
    )
    text = str(digits)
    if len(text) > width:
        raise RadarConversionError(
            f"{obj} {epoch}: {field}={value} overflows find_orb's "
            f"{width}-character field (would need {len(text)} digits)"
        )
    return text.rjust(width, "0")


def pack_permid(obj: str, epoch: str, permid: str) -> str:
    """ADES numbered permID -> the 12-byte MPC packed designation.

    ``create_mpc_packed_desig`` in find_orb renders a numbered minor planet
    below 620000 as ``"%c%04d       "`` with the high digits in mutant hex, and
    find_orb's own ADES reader reaches the same 12 bytes for the optical rows —
    which is what makes the radar records attach to the same object as the
    optical table rather than becoming a second object.
    """
    text = permid.strip()
    if not text.isdigit():
        raise RadarConversionError(
            f"{obj} {epoch}: permID={permid!r} is not a numbered designation; "
            "the packed-designation encoding here only covers numbered minor "
            "planets, and guessing a provisional packing would silently split "
            "the radar off into its own object"
        )
    number = int(text)
    if not 0 < number < 620000:
        raise RadarConversionError(
            f"{obj} {epoch}: permID={number} is outside the range this packer "
            "covers (1..619999)"
        )
    return "%c%04d       " % (MUTANT_HEX[number // 10000], number % 10000)


def pack_obstime(obj: str, epoch: str) -> str:
    """ISO ``obsTime`` -> the 14-byte ``CYYMMDD:HHMMSS`` field at columns 16-29.

    This is exactly what find_orb's own ADES translator emits for radar
    (``ades2mpc.cpp`` move_fits_time: copy the digits, turn ``T`` into ``:``,
    stop at ``Z``), and it is exact by construction. The alternative
    ``YYYY MM DD.dddddd`` micro-day form parses to a byte-identical orbit, but
    only because ``extract_date_from_mpc_report`` ROUNDS every radar time to
    the nearest UTC second; the sexagesimal form does not depend on that.

    Fractional seconds are refused rather than rounded: find_orb would round
    them away silently, and a radar epoch we cannot represent exactly is a
    thing the owner needs told, not a thing to paper over. Every radar epoch in
    the current fixtures falls on a whole minute.
    """
    t = epoch.strip()
    ok = (
        len(t) >= 20
        and t[4] == "-" and t[7] == "-" and t[10] == "T"
        and t[13] == ":" and t[16] == ":" and t.endswith("Z")
    )
    if not ok:
        raise RadarConversionError(
            f"{obj}: obsTime={epoch!r} is not ISO ``YYYY-MM-DDTHH:MM:SSZ``"
        )
    if t[19:-1] not in ("", ".0", ".00", ".000"):
        raise RadarConversionError(
            f"{obj} {epoch}: radar epoch carries fractional seconds; the "
            "80-column sexagesimal field holds whole seconds only and find_orb "
            "rounds radar times to the nearest second regardless"
        )
    century = int(t[0:2])
    return (
        MUTANT_HEX[century] + t[2:4] + t[5:7] + t[8:10]
        + ":" + t[11:13] + t[14:16] + t[17:19]
    )


# ── Row merging ─────────────────────────────────────────


def merge_radar_rows(obj: str, header: List[str], lines: List[str]) -> List[Dict[str, str]]:
    """Collapse each epoch's delay row and doppler row into one measurement.

    ADES gives radar one quantity per row: across all five fixtures, 90 rows
    carry a delay and 66 carry a doppler, and NO row carries both. The MPC
    80-column record holds both, so the two rows for one epoch become one R/r
    pair here.

    find_orb would do this itself (``mpc_obs.cpp:3038 fix_radar_obs`` merges
    adjacent radar observations at an identical JD), but merging on our side is
    strictly better for three reasons:

      * fix_radar_obs copies only 14 characters at offsets 33 and 47, so the
        doppler field's last byte (its 1e-4 Hz digit) is never transferred and
        reads as ``0``. Self-merging is exact.
      * fix_radar_obs does not compare station codes and unconditionally frees
        and drops the second record. Two records that happen to share an epoch
        collapse with NO message and exit code 0.
      * it depends on the two records being adjacent after find_orb's sort.

    Guards here are all fatal: two delays at one epoch, or two rows at one
    epoch that disagree on station / frequency / centre-of-mass flag, mean the
    fixture is not what this converter assumes and the result would be a fit
    built on a measurement we invented.
    """
    if "obsTime" not in header:
        raise RadarConversionError(f"{obj}: radar table has no obsTime column")

    merged: Dict[str, Dict[str, str]] = {}
    order: List[str] = []
    # Columns that identify the measurement's geometry rather than its value;
    # every row at one epoch must agree on all of them.
    context = [c for c in ("permID", "trx", "rcv", "com", "frq") if c in header]

    for line in lines:
        row = {k: v.strip() for k, v in zip(header, line.split("|"))}
        epoch = row.get("obsTime", "")
        if not epoch:
            raise RadarConversionError(f"{obj}: radar row with empty obsTime: {line[:60]!r}")
        if epoch not in merged:
            merged[epoch] = dict(row)
            order.append(epoch)
            continue
        prior = merged[epoch]
        for col in context:
            if prior.get(col, "") != row.get(col, ""):
                raise RadarConversionError(
                    f"{obj} {epoch}: two radar rows at one epoch disagree on "
                    f"{col} ({prior.get(col)!r} vs {row.get(col)!r}); find_orb "
                    "stores one station pair and one frequency per record, so "
                    "these cannot be merged"
                )
        for col in ("delay", "rmsDelay", "doppler", "rmsDoppler"):
            value = row.get(col, "")
            if not value:
                continue
            if prior.get(col, ""):
                raise RadarConversionError(
                    f"{obj} {epoch}: two radar rows at one epoch both carry "
                    f"{col} ({prior[col]!r}, {value!r}); the 80-column record "
                    "holds one of each and find_orb would drop one silently"
                )
            prior[col] = value

    return [merged[e] for e in order]


# ── Record emission ─────────────────────────────────────


def emit_radar_record(obj: str, row: Dict[str, str]) -> Tuple[str, str]:
    """One merged radar measurement -> the ``R``/``r`` 80-column line pair.

    Column map, 0-indexed, read off find_orb's parser. Both lines must carry
    the SAME designation, the SAME 16 date bytes and the SAME receiver code
    with only the R/r case differing — that is literally ``matching_lines()``.
    A first line whose second line does not match is dropped with a stdout
    warning and EXIT CODE 0, so the caller must reconcile radar counts.
    """
    epoch = row.get("obsTime", "<no obsTime>")
    line1 = [" "] * 80
    line2 = [" "] * 80

    desig = pack_permid(obj, epoch, row.get("permID", ""))
    when = pack_obstime(obj, epoch)

    trx = row.get("trx", "").strip()
    rcv = row.get("rcv", "").strip()
    # trx is the TRANSMITTING station (columns 69-71) and rcv the RECEIVING
    # station (columns 78-80). find_orb models the bistatic geometry properly:
    # compute_radar_info reads second_line+68 to place the transmitter at the
    # up-leg epoch and uses obs->mpc_code (= columns 78-80, the receiver) for
    # the down-leg, so obsTime is the RECEIVE epoch. Toutatis is the only
    # bistatic fixture (trx=253 Goldstone DSS-14, rcv=252 DSS-13, 11 rows);
    # swapping the two codes measurably degrades exactly those rows.
    for code, name in ((trx, "trx"), (rcv, "rcv")):
        if not code or len(code) > 3:
            raise RadarConversionError(
                f"{obj} {epoch}: {name}={code!r} is not a 1-3 character MPC "
                "observatory code"
            )

    for line, note2 in ((line1, "R"), (line2, "r")):
        line[0:12] = desig
        # [12] is the discovery/exclusion flag: find_orb accepts ' ', '*' or
        # '-', and '-' would mark the observation EXCLUDED from the fit.
        line[14] = note2                      # mpc_obs.cpp:1717 radar marker
        line[15:29] = when
        line[68:71] = "%-3s" % trx
        # [71:77] is the reference field. It must stay blank: a leading '!'
        # there puts find_orb into its "private observation" prompt.
        line[77:80] = "%-3s" % rcv

    # Transmitter frequency, MHz. Five integer digits at [62:67] plus a tenths
    # digit at [67]; compute_radar_info assembles "NNNNN.N" + six more digits
    # from line 2 and multiplies by 1e6.
    #
    # ZERO-padded, not space-padded, and this is not cosmetic: find_orb re-scans
    # columns 57-65 of EVERY record with ``sscanf("%lf %lf%n")`` looking for an
    # optional "ra_sigma dec_sigma" override. On a radar record those bytes are
    # the tail of the doppler field followed by the frequency; a leading space
    # in the frequency makes that scan succeed and overwrite posn_sigma_1/2 with
    # garbage, which prints the doppler residual as ``----``. Zero-padding
    # removes the space so the scan returns 1 and the override never fires.
    # (Display-only — the fitted elements were byte-identical either way — but a
    # residual listing with ``----`` where a number belongs gets misread later.)
    if "frq" not in row or not row["frq"].strip():
        raise RadarConversionError(
            f"{obj} {epoch}: no transmitter frequency; find_orb needs it to "
            "turn the doppler shift into a range rate"
        )
    frq = _decimal(obj, epoch, "frq", row["frq"])
    if frq <= 0:
        raise RadarConversionError(f"{obj} {epoch}: frq={frq} is not positive")
    frq_digits = _scaled_digits(obj, epoch, "frq", frq, 1, W_FRQ)  # MHz * 10
    line1[62:68] = frq_digits

    # Centre-of-mass flag -> 'C' / 'S' at line 2 column 33.
    #
    # ADES com=1 means the measurement is reduced to the target's centre of
    # mass; com=0 means the peak-power / leading-edge position, modelled one
    # object radius BEFORE the centre of mass.
    #
    # find_orb transports this byte but IGNORES it: extract_radar_value skips
    # position 0 of the sigma field, and nothing else in find_orb reads
    # second_line[32] for a radar record. compute_radar_info always computes the
    # round-trip time to obs->obj_posn, the centre of mass — i.e. it assumes
    # com=1 unconditionally. So a com=0 DELAY row fed to find_orb carries an
    # unmodelled 2R/c bias (Bennu's ~250 m radius is 1.7 us, comparable to its
    # own 0.5-10 us quoted sigma). That is a real bias, not a rounding error, so
    # it is a hard failure below rather than something to emit and hope about.
    #
    # The flag is still written faithfully for provenance and for any future
    # consumer that does honour it.
    com = row.get("com", "").strip()
    if com not in ("0", "1"):
        raise RadarConversionError(
            f"{obj} {epoch}: com={com!r} is neither 0 (leading edge) nor 1 "
            "(centre of mass); find_orb always models centre of mass, so an "
            "unknown reduction point cannot be converted safely"
        )
    line2[32] = "C" if com == "1" else "S"

    delay_text = row.get("delay", "").strip()
    rms_delay_text = row.get("rmsDelay", "").strip()
    doppler_text = row.get("doppler", "").strip()
    rms_doppler_text = row.get("rmsDoppler", "").strip()

    if not delay_text and not doppler_text:
        raise RadarConversionError(
            f"{obj} {epoch}: radar row carries neither delay nor doppler"
        )

    if delay_text:
        if com != "1":
            raise RadarConversionError(
                f"{obj} {epoch}: delay row has com=0 (leading edge). find_orb "
                "models the delay to the centre of mass and ignores the flag, "
                "so this observation would enter the fit biased by roughly "
                "2R/c with nothing to warn you"
            )
        if not rms_delay_text:
            raise RadarConversionError(
                f"{obj} {epoch}: delay={delay_text} has no rmsDelay. find_orb "
                "divides by the sigma; a blank one aborts the fit in "
                "full_improvement (assert(matrix), rc=134)"
            )
        delay = _decimal(obj, epoch, "delay", delay_text)
        rms_delay = _decimal(obj, epoch, "rmsDelay", rms_delay_text)
        # A value of EXACTLY zero is indistinguishable from an absent one:
        # compute_radar_info's consumers test ``if (!rinfo.doppler_obs)``.
        if delay <= 0:
            raise RadarConversionError(
                f"{obj} {epoch}: delay={delay} must be positive (round-trip "
                "light time); zero is how find_orb spells 'absent'"
            )
        if rms_delay <= 0:
            raise RadarConversionError(
                f"{obj} {epoch}: rmsDelay={rms_delay} must be positive; a "
                "non-positive sigma trips find_orb's own assert at "
                "mpc_obs.cpp:5194 (SIGABRT)"
            )
        # ADES delay is SECONDS (adesmaster.xml:1610); find_orb reads this
        # field as MICROSECONDS with an implied decimal 4 digits from the
        # right, so the integer written here is delay_seconds * 1e6 * 1e4.
        line1[32:47] = _scaled_digits(obj, epoch, "delay", delay, 10, W_DELAY)
        # ADES rmsDelay is already MICROSECONDS (adesmaster.xml:1611) — the
        # mismatch with `delay` is deliberate in the spec — so only the 1e-4
        # implied decimal has to be undone.
        line2[33:47] = _scaled_digits(
            obj, epoch, "rmsDelay", rms_delay, 4, W_RMS_DELAY
        )
    elif rms_delay_text:
        raise RadarConversionError(
            f"{obj} {epoch}: rmsDelay={rms_delay_text} with no delay to attach "
            "it to"
        )

    if doppler_text:
        if not rms_doppler_text:
            raise RadarConversionError(
                f"{obj} {epoch}: doppler={doppler_text} has no rmsDoppler; "
                "find_orb weights by it and cannot fit without it"
            )
        doppler = _decimal(obj, epoch, "doppler", doppler_text)
        rms_doppler = _decimal(obj, epoch, "rmsDoppler", rms_doppler_text)
        if doppler == 0:
            raise RadarConversionError(
                f"{obj} {epoch}: doppler is exactly zero, which find_orb "
                "cannot distinguish from an absent measurement"
            )
        if rms_doppler <= 0:
            raise RadarConversionError(
                f"{obj} {epoch}: rmsDoppler={rms_doppler} must be positive; a "
                "non-positive sigma trips find_orb's assert at "
                "mpc_obs.cpp:5194 (SIGABRT)"
            )
        # ADES doppler and rmsDoppler are both Hz (adesmaster.xml:1612-1613),
        # and find_orb's field is Hz with the 1e-4 implied decimal. The sign
        # gets its own byte at [47] because extract_radar_value reads it there
        # (``if buff[0] == '-': rval *= -1``) rather than from the digits.
        line1[47] = "-" if doppler < 0 else "+"
        line1[48:62] = _scaled_digits(
            obj, epoch, "doppler", doppler, 4, W_DOPPLER
        )
        line2[47:62] = _scaled_digits(
            obj, epoch, "rmsDoppler", rms_doppler, 4, W_RMS_DOPPLER
        )
    elif rms_doppler_text:
        raise RadarConversionError(
            f"{obj} {epoch}: rmsDoppler={rms_doppler_text} with no doppler to "
            "attach it to"
        )

    out1, out2 = "".join(line1), "".join(line2)
    # extract_date_from_mpc_report refuses a record whose strlen is outside
    # [80, 82], so a slipped field would become a wordless "not an observation".
    if len(out1) != 80 or len(out2) != 80:
        raise RadarConversionError(
            f"{obj} {epoch}: emitted {len(out1)}/{len(out2)} characters, not 80"
        )
    return out1, out2


def _epoch_seconds(epoch: str) -> float:
    """Seconds-from-epoch for the duplicate-proximity check only."""
    return datetime.strptime(
        epoch.strip().replace("Z", ""), "%Y-%m-%dT%H:%M:%S"
    ).replace(tzinfo=timezone.utc).timestamp()


def convert_radar_psv(obj: str, psv: str) -> Tuple[str, List[str]]:
    """Split a psv-radar fixture into optical PSV text plus 80-column radar.

    Returns ``(optical_psv, radar_lines)``. ``optical_psv`` is a complete,
    single-table ADES PSV document — feed it through ``sanitize_psv`` (which
    is only correct on a single table) and then append ``radar_lines``.
    ``radar_lines`` has two entries per measurement, the ``R`` line then its
    ``r`` line.

    A fixture with no radar table returns an empty ``radar_lines``; that is a
    fixture fact, not a failure. A fixture with a radar table that cannot be
    converted raises.
    """
    tables = split_psv_tables(psv)
    if not tables:
        raise RadarConversionError(f"{obj}: no PSV tables found")

    radar_idx = find_radar_table(tables)
    optical_tables = [t for i, t in enumerate(tables) if i != radar_idx]
    if len(optical_tables) != 1:
        raise RadarConversionError(
            f"{obj}: expected exactly one non-radar table, found "
            f"{len(optical_tables)}"
        )

    _, opt_lines, opt_raw_header = optical_tables[0]
    optical_psv = "# version=2017\n" + opt_raw_header + "\n"
    optical_psv += "".join(ln + "\n" for ln in opt_lines)

    if radar_idx is None:
        return optical_psv, []

    header, lines, _ = tables[radar_idx]
    rows = merge_radar_rows(obj, header, lines)

    # find_orb collapses two observations from the same station less than 3
    # seconds apart into one (times_very_close), WITHOUT a message. Our merge
    # keys on the exact obsTime string, so near-but-not-equal epochs would
    # survive here and vanish inside find_orb.
    by_station: Dict[str, List[Tuple[float, str]]] = {}
    for row in rows:
        by_station.setdefault(row.get("rcv", ""), []).append(
            (_epoch_seconds(row["obsTime"]), row["obsTime"])
        )
    for station, stamps in by_station.items():
        stamps.sort()
        for (t0, e0), (t1, e1) in zip(stamps, stamps[1:]):
            if t1 - t0 < DUPLICATE_THRESHOLD_S:
                raise RadarConversionError(
                    f"{obj}: radar epochs {e0} and {e1} at station {station} "
                    f"are {t1 - t0:.3f} s apart, inside find_orb's {DUPLICATE_THRESHOLD_S} s "
                    "duplicate window; it would drop one without saying so"
                )

    radar_lines: List[str] = []
    for row in rows:
        first, second = emit_radar_record(obj, row)
        radar_lines.append(first)
        radar_lines.append(second)
    return optical_psv, radar_lines


if __name__ == "__main__":
    import pathlib
    import sys

    for path in sys.argv[1:]:
        p = pathlib.Path(path)
        optical, radar = convert_radar_psv(p.stem, p.read_text())
        out = p.with_suffix(".radar80")
        out.write_text("".join(ln + "\n" for ln in radar))
        print(
            f"{p.stem}: {len(optical.splitlines()) - 2} optical rows, "
            f"{len(radar) // 2} radar records -> {out}"
        )
