/* empyrean validation runner — C channel.
 *
 * Tests the public C API directly (libempyrean.dylib + empyrean.h).
 * Reads space-separated command lines from stdin and writes results
 * to stdout. Driven by validation/runners/c/drive.py.
 *
 * Four modes, dispatched on the first whitespace-separated token:
 *
 *   prop EPOCH IC[6] A1 A2 A3 G[5] FORCE TARGET [METHOD]
 *     → ok x y z vx vy vz time_ms
 *       The optional 19th METHOD token is the plan row's
 *       `<method>_<arm>` uncertainty tag (every covariance-bearing method;
 *       `none` / the untagged legacy row send the bare 18-field line). With a
 *       token, the synthetic input covariance is attached, the rung set, and
 *       the delivered 0.11 products appended as 12 `key=value` tokens:
 *         → ok x y z vx vy vz time_ms resolved_method=… cov_kind=… \
 *              cov_joint_width=… orbit_delivered=… orbit_status=… \
 *              mix_n_components_total=… … mix_n_sky_linearization_refused=… \
 *              cov_tri=c0,c1,…
 *       read off the delivered packed joint (`orbit_cov`), the per-orbit
 *       outcome (`outcomes[0]`), and — for a mixture — `mixture_tally`. An
 *       absent scalar / triangle renders as the literal `na`.
 *
 *   eph EPOCH IC[6] A1 A2 A3 G[5] FORCE TARGET OBS_CODE [METHOD]
 *     → ok ra_deg dec_deg rho_au lt_d time_ms
 *       The optional 20th METHOD token (after the observer) carries the plan
 *       row's `<method>_<arm>` uncertainty tag, exactly like the prop line's
 *       19th token: with a token the synthetic covariance is attached, the rung
 *       set, and the same 12 `key=value` product tokens appended after
 *       `… time_ms` — read off the delivered ephemeris ENTRY's own packed joint
 *       (`entry.joint`), the per-orbit outcome (`outcomes[0]`), and the entry's
 *       PRE-RETENTION `mixture_tally`. Without a token the line stays the
 *       covariance-free 5-field form, byte-identical to before.
 *
 *   od FORCE EXCLUDE_NAIF METHOD ADES_PATH   (path is last; may contain
 *                                             spaces; EXCLUDE_NAIF=0 → no
 *                                             exclusion)
 *       METHOD = `-` → the legacy untagged optical fit:
 *         → ok x y z vx vy vz iterations time_ms rms_combined
 *       METHOD = `<method>_<arm>` → a per-method fit binding
 *       `ODConfig.uncertainty_method`. FirstOrder / Auto deliver with the 12
 *       product tokens appended after `… iterations time_ms`; every richer
 *       method is refused by the engine BY NAME:
 *         → refused <engine text naming the method>
 *
 *   odng FORCE EXCLUDE_NAIF ADES_PATH  (the legacy untagged fit's non-grav
 *                                       recovery second pass, solve_for =
 *                                       StateAndNonGrav; no method axis)
 *     → ok a1 a2 a3 a1_sigma a2_sigma a3_sigma iterations time_ms
 *       a* / a*_sigma are emitted as the token "null" when non-grav was
 *       not actually recovered (has_covariance_9x9 == 0, or a non-finite
 *       fitted value) — see the None/NaN guard below.
 *
 * On any failure: fail <message>
 *
 * usage: runner [data_dir]
 *   data_dir defaults to ~/.empyrean/data/.
 */

#include "empyrean.h"

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000.0 + ts.tv_nsec / 1.0e6;
}

/* Build a default-shaped EmpyreanPropagationConfig (force_model + frame
 * set; everything else zeroed but with NaN sentinels for fields where 0
 * is the wrong choice — matches the wrapper's PropagationConfig::default(). */
static void build_prop_config(struct EmpyreanPropagationConfig *cfg, int force_model) {
    memset(cfg, 0, sizeof(*cfg));
    cfg->force_model = force_model;
    cfg->uncertainty_method.tag = EMPYREAN_UNCERTAINTY_FIRST;
    cfg->frame = 0; /* ICRF */
    /* Per-row fork-exec runner: pin to 1 thread so the driver running
     * many of these in parallel doesn't blow ulimit -u (was hitting
     * EAGAIN on Rayon pool init). */
    cfg->num_threads = 1;
    cfg->advanced.dt_initial = NAN;
    cfg->advanced.dt_min = NAN;
    /* empyrean-4zjo: origin_switching.enabled is now tri-state (0 = use
     * upstream default = ON, 1 = force ON, 2 = force OFF). The memset(0)
     * above leaves it as DEFAULT, which the cdylib resolves to the
     * upstream default — currently ON — without any explicit set
     * required here. Matches the dt_min / dt_initial / hysteresis
     * sentinel convention. Leaving this line in place commented as
     * documentation in case a future debugger wants to force ON for an
     * isolation test: `cfg->advanced.origin_switching.enabled =
     * EMPYREAN_ORIGIN_SWITCHING_ON;` */
}

static void build_orbit(struct EmpyreanOrbit *orbit,
                        double epoch, const double pos[3], const double vel[3],
                        double a1, double a2, double a3,
                        const double g[5]) {
    memset(orbit, 0, sizeof(*orbit));
    orbit->state.epoch_mjd_tdb = epoch;
    orbit->state.elements[0] = pos[0];
    orbit->state.elements[1] = pos[1];
    orbit->state.elements[2] = pos[2];
    orbit->state.elements[3] = vel[0];
    orbit->state.elements[4] = vel[1];
    orbit->state.elements[5] = vel[2];
    orbit->state.has_covariance = 0;
    orbit->state.representation = 0; /* Cartesian */
    orbit->state.frame = 0;          /* ICRF */
    orbit->state.origin = 0;         /* SSB */
    orbit->a1 = a1;
    orbit->a2 = a2;
    orbit->a3 = a3;
    orbit->ng_alpha = g[0];
    orbit->ng_r0 = g[1];
    orbit->ng_m = g[2];
    orbit->ng_n = g[3];
    orbit->ng_k = g[4];
}

/* ── 0.11 per-method uncertainty binding ──────────────────────────────────
 *
 * The 0.11 C ABI carries the uncertainty axis the rust / python / cli channels
 * carry: the propagation / OD config takes an `EmpyreanUncertaintyMethod`, the
 * delivered state carries a self-describing packed joint (`orbit_cov`) plus the
 * per-orbit outcome (`outcomes[0]`) and, for a mixture, the pre-retention
 * tallies (`mixture_tally`). This block binds those surfaces so the C channel
 * produces the same per-method products the cli binary does
 * (runners/cli/src/main.rs) — read what the engine DELIVERED, never the
 * request. The driver (runners/c/drive.py) is the parity reference for the
 * schema field NAMES; the token wire here mirrors the cli binary's
 * `Products::render`.
 */

/* Monte-Carlo sample count and seed — a LITERAL mirror of
 * empyrean_validation::schema::uncertainty_modes::{MONTE_CARLO_SAMPLE_COUNT,
 * MONTE_CARLO_SEED} (src/schema.rs). Fixed suite-wide so a seeded Monte-Carlo
 * row is a cross-channel bit check; never the engine's per-call convenience
 * seed. */
#define EMPYREAN_VALIDATION_MC_SAMPLES 100u
#define EMPYREAN_VALIDATION_MC_SEED 0x454D50595245414EULL

/* The suite's synthetic 6×6 Cartesian covariance — 1 km position σ, 1 mm/s
 * velocity σ, uncorrelated — as the 21-double packed LOWER triangle, index
 * (i,j) → i·(i+1)/2 + j for j ≤ i. Byte-identical to the cli binary's
 * `synthetic_covariance` (and the rust channel's `build_uncertainty_axes`), so
 * a same-method cross-channel compare sees the same input uncertainty. Units
 * are AU and AU/day; the diagonal entries land at i·(i+1)/2 + i. */
static void synthetic_cov_tri(double tri[21]) {
    const double pos_sigma_au = 1.0 / 149597870.700;
    const double vel_sigma_au_d = 1e-6 / 149597870.700 * 86400.0;
    const double pos_var = pos_sigma_au * pos_sigma_au;
    const double vel_var = vel_sigma_au_d * vel_sigma_au_d;
    for (int k = 0; k < 21; k++) {
        tri[k] = 0.0;
    }
    tri[0] = pos_var;  /* (0,0) */
    tri[2] = pos_var;  /* (1,1) */
    tri[5] = pos_var;  /* (2,2) */
    tri[9] = vel_var;  /* (3,3) */
    tri[14] = vel_var; /* (4,4) */
    tri[20] = vel_var; /* (5,5) */
}

/* Attach the synthetic state-only covariance to an orbit as the single packed
 * joint — layout_present = 0 (no parameter axes), width 6, tri_len 21. The
 * `tri` is caller-owned and borrowed for the call; the engine re-derives kind /
 * quality on ingestion, so the tagged side columns here are the not-asserted
 * defaults. Mirrors the wrapper building the joint from a 6×6. */
static void attach_synthetic_joint(struct EmpyreanOrbit* orbit, double tri[21]) {
    synthetic_cov_tri(tri);
    orbit->covariance.layout_present = 0;
    orbit->covariance.layout_considered = 0;
    orbit->covariance.layout_marginalized = 0;
    orbit->covariance.width = 6;
    orbit->covariance.tri = tri;
    orbit->covariance.tri_len = 21;
    orbit->covariance.kind = 0;    /* re-derived on ingestion */
    orbit->covariance.quality = 0; /* re-derived on ingestion */
    orbit->covariance.cov_rank = 0;
    orbit->covariance.cov_null_axes = 0;
    orbit->covariance.cov_min_eig_corr = NAN;
    orbit->covariance.functional = EMPYREAN_JOINT_FUNCTIONAL_STATE;
    orbit->covariance.provenance = EMPYREAN_JOINT_PROVENANCE_DERIVED;
    orbit->covariance.asserted_source = EMPYREAN_JOINT_ASSERTED_SOURCE_NONE;
}

/* The method vocabulary, a LITERAL mirror of
 * empyrean_validation::schema::uncertainty_modes (src/schema.rs): the seven
 * method prefixes a composite `<method>_<arm>` tag can carry. The driver keys
 * the rung on the prefix alone (the arm is irrelevant to which rung the engine
 * runs), so the method is read off the longest matching prefix bounded by `_`
 * or end-of-string — `second_order_detection_on` reads as `second_order`,
 * never `second`. */
static const char* const UNCERTAINTY_METHODS[] = {
    "none",        "first_order",  "second_order", "auto",
    "sigma_point", "monte_carlo",  "gaussian_mixture",
};

/* Map a composite tag's method PREFIX to `(attach, flat UncertaintyMethod)`,
 * mirroring the cli binary's `method_for_tag` + the wrapper's
 * `UncertaintyMethod::to_ffi`. `none` is the covariance-free first-order path
 * (no covariance attached); every other method attaches the synthetic
 * covariance and runs its named rung. Returns 1 on success (fills `*attach` and
 * `*out`), 0 for a tag whose prefix is outside the vocabulary — the caller then
 * refuses it by name rather than silently substituting a method. */
static int method_for_tag(const char* tag, int* attach,
                          struct EmpyreanUncertaintyMethod* out) {
    const char* method = NULL;
    for (size_t i = 0; i < sizeof(UNCERTAINTY_METHODS) / sizeof(*UNCERTAINTY_METHODS);
         i++) {
        const char* m = UNCERTAINTY_METHODS[i];
        size_t len = strlen(m);
        if (strncmp(tag, m, len) == 0 && (tag[len] == '_' || tag[len] == '\0')) {
            method = m;
            break;
        }
    }
    if (!method) {
        return 0;
    }
    memset(out, 0, sizeof(*out));
    if (strcmp(method, "none") == 0) {
        *attach = 0;
        out->tag = EMPYREAN_UNCERTAINTY_FIRST;
    } else if (strcmp(method, "first_order") == 0) {
        *attach = 1;
        out->tag = EMPYREAN_UNCERTAINTY_FIRST;
    } else if (strcmp(method, "second_order") == 0) {
        *attach = 1;
        out->tag = EMPYREAN_UNCERTAINTY_SECOND;
    } else if (strcmp(method, "auto") == 0) {
        *attach = 1;
        out->tag = EMPYREAN_UNCERTAINTY_AUTO;
        out->auto_threshold_first = 0.1;
        out->auto_threshold_mixture = 10.0;
        out->auto_threshold_ip_skip = 1e-12;
        out->auto_gmm_max_depth = 3;
        out->auto_gmm_components_per_split = 3;
    } else if (strcmp(method, "sigma_point") == 0) {
        *attach = 1;
        out->tag = EMPYREAN_UNCERTAINTY_SIGMA_POINT;
        out->sp_n_sigma = 1.0;
        out->sp_samples_per_plane = 8;
    } else if (strcmp(method, "monte_carlo") == 0) {
        *attach = 1;
        out->tag = EMPYREAN_UNCERTAINTY_MONTE_CARLO;
        out->mc_n_samples = EMPYREAN_VALIDATION_MC_SAMPLES;
        out->mc_seed_some = 1;
        out->mc_seed = EMPYREAN_VALIDATION_MC_SEED;
    } else { /* gaussian_mixture */
        *attach = 1;
        out->tag = EMPYREAN_UNCERTAINTY_MIXTURE;
        out->auto_threshold_mixture = 1.0;
        out->auto_gmm_max_depth = 3;
        out->auto_gmm_components_per_split = 3;
    }
    return 1;
}

/* The bare method name a delivered covariance KIND reports as `resolved_method`
 * — the kind the engine delivered, never the request, so a silent substitution
 * under an explicit method is caught by the cross-channel compare. The driver
 * composes it with the row's arm. Mirrors the cli binary's
 * `resolved_method_tag`. An unrecognized kind yields NULL → the `na` token. */
static const char* resolved_method_name(uint8_t kind) {
    switch (kind) {
    case EMPYREAN_COVARIANCE_KIND_LINEAR:
        return "first_order";
    case EMPYREAN_COVARIANCE_KIND_SECOND_ORDER:
        return "second_order";
    case EMPYREAN_COVARIANCE_KIND_MIXTURE:
        return "gaussian_mixture";
    case EMPYREAN_COVARIANCE_KIND_MONTE_CARLO:
        return "monte_carlo";
    case EMPYREAN_COVARIANCE_KIND_SIGMA_POINT:
        return "sigma_point";
    default:
        return NULL;
    }
}

/* Name an `EMPYREAN_PROPAGATE_FAILURE_*` classification code, mirroring the cli
 * / rust channels' `propagate_failure_variant`; an unrecognized code falls back
 * to the engine's own message so no failure is ever a bare number. Writes into
 * `dst`. */
static void propagate_failure_variant(char* dst, size_t dstsz, int32_t code,
                                      const char* message) {
    const char* name = NULL;
    switch (code) {
    case EMPYREAN_PROPAGATE_FAILURE_INTEGRATION:
        name = "integration";
        break;
    case EMPYREAN_PROPAGATE_FAILURE_KEPLER_DT_BACKPROP:
        name = "kepler_dt_backprop";
        break;
    case EMPYREAN_PROPAGATE_FAILURE_TRANSFORM:
        name = "transform";
        break;
    case EMPYREAN_PROPAGATE_FAILURE_COVARIANCE_INPUT:
        name = "covariance_input";
        break;
    case EMPYREAN_PROPAGATE_FAILURE_SIGMA_POINT:
        name = "sigma_point";
        break;
    case EMPYREAN_PROPAGATE_FAILURE_SAMPLED_PARAMETER:
        name = "sampled_parameter";
        break;
    case EMPYREAN_PROPAGATE_FAILURE_ENSEMBLE_MEMBER:
        name = "ensemble_member";
        break;
    case EMPYREAN_PROPAGATE_FAILURE_OUTPUT_ASSEMBLY:
        name = "output_assembly";
        break;
    case EMPYREAN_PROPAGATE_FAILURE_OTHER:
        name = "other";
        break;
    default:
        break;
    }
    if (name) {
        snprintf(dst, dstsz, "%s", name);
    } else {
        snprintf(dst, dstsz, "code_%d(%s)", (int)code, message ? message : "");
    }
}

/* Collapse any run of whitespace in `src` to a single `_` (and strip leading /
 * trailing whitespace) so an `orbit_status` carrying the engine's free-form
 * withheld / failure text stays a single whitespace-free token — exactly as
 * the cli binary's `split_whitespace().join("_")`. */
static void sanitize_status(char* dst, size_t dstsz, const char* src) {
    size_t j = 0;
    int started = 0;
    int pending_ws = 0;
    for (size_t i = 0; src[i]; i++) {
        char c = src[i];
        int ws = (c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f'
                  || c == '\v');
        if (ws) {
            if (started) {
                pending_ws = 1;
            }
            continue;
        }
        if (pending_ws && j + 1 < dstsz) {
            dst[j++] = '_';
        }
        pending_ws = 0;
        if (j + 1 < dstsz) {
            dst[j++] = c;
        }
        started = 1;
    }
    dst[j] = '\0';
}

/* The 0.11 per-method products carried on a propagation or OD fit row, read off
 * the engine-delivered result (never recomputed). Absent scalars render as the
 * `na` token the driver maps to null; the packed triangle is a comma-separated
 * list of `%.18e` doubles. The i64 mixture tallies carry `-1` when the
 * producing site did not carry them — rendered `na` (absent), never a
 * fabricated 0. */
struct Products {
    const char* resolved_method; /* NULL → na */
    int has_cov_kind;
    uint8_t cov_kind;
    int has_cov_width;
    uint32_t cov_width;
    int orbit_delivered;
    char orbit_status[512]; /* already sanitized to a single token */
    int has_mix;
    uint32_t mix_total;
    double mix_weight;
    int64_t mix_failed;
    int64_t mix_unresolved;
    int64_t mix_curvature;
    int64_t mix_sky;
    double tri[210]; /* copied out of the (library-owned) joint; width ≤ 20 */
    uintptr_t tri_len; /* 0 → na */
};

/* Print one `-1 → na` mixture tally token. */
static void print_mix_i64(const char* key, int64_t v) {
    if (v < 0) {
        printf(" %s=na", key);
    } else {
        printf(" %s=%lld", key, (long long)v);
    }
}

/* Render the 12 per-method product tokens (no leading space, no trailing
 * newline) in the cli binary's `Products::render` order. The token keys ARE the
 * schema field names, so the driver maps each straight onto the JSON row. */
static void render_products(const struct Products* p) {
    printf("resolved_method=%s", p->resolved_method ? p->resolved_method : "na");
    if (p->has_cov_kind) {
        printf(" cov_kind=%u", (unsigned)p->cov_kind);
    } else {
        printf(" cov_kind=na");
    }
    if (p->has_cov_width) {
        printf(" cov_joint_width=%u", (unsigned)p->cov_width);
    } else {
        printf(" cov_joint_width=na");
    }
    printf(" orbit_delivered=%d", p->orbit_delivered ? 1 : 0);
    printf(" orbit_status=%s", p->orbit_status);
    if (p->has_mix) {
        printf(" mix_n_components_total=%u", (unsigned)p->mix_total);
        printf(" mix_weight_delivered=%.18e", p->mix_weight);
        print_mix_i64("mix_n_failed", p->mix_failed);
        print_mix_i64("mix_n_unresolved", p->mix_unresolved);
        print_mix_i64("mix_n_curvature_refused", p->mix_curvature);
        print_mix_i64("mix_n_sky_linearization_refused", p->mix_sky);
    } else {
        printf(" mix_n_components_total=na mix_weight_delivered=na mix_n_failed=na"
               " mix_n_unresolved=na mix_n_curvature_refused=na"
               " mix_n_sky_linearization_refused=na");
    }
    if (p->tri_len > 0) {
        printf(" cov_tri=");
        for (uintptr_t i = 0; i < p->tri_len; i++) {
            printf(i == 0 ? "%.18e" : ",%.18e", p->tri[i]);
        }
    } else {
        printf(" cov_tri=na");
    }
}

/* Capture the 0.11 per-method products off a delivered propagation result
 * (`states[0].orbit_cov`, `outcomes[0]`, `states[0].mixture_tally`) BEFORE the
 * result is freed — the joint's `tri` is library-owned, so copy it out. `attach`
 * is 1 when a covariance-bearing method was requested, so a delivered row whose
 * joint is absent reads `cov_withheld:…` rather than silently dropping it. */
static void capture_prop_products(struct Products* p,
                                  const struct EmpyreanPropagationResult* res,
                                  int attach) {
    memset(p, 0, sizeof(*p));
    const struct EmpyreanPackedJoint* j = &res->states[0].orbit_cov;
    int have_cov = (j->width > 0 && j->tri != NULL);
    if (have_cov) {
        p->resolved_method = resolved_method_name(j->kind);
        p->has_cov_kind = 1;
        p->cov_kind = j->kind;
        p->has_cov_width = 1;
        p->cov_width = j->width;
        p->tri_len = (j->tri_len > 210) ? 210 : j->tri_len;
        for (uintptr_t i = 0; i < p->tri_len; i++) {
            p->tri[i] = j->tri[i];
        }
    }
    /* Per-orbit delivery outcome read off outcomes[0] — the sole delivery
     * discriminator, never a row count. */
    const struct EmpyreanOrbitOutcome* oc = res->outcomes ? &res->outcomes[0] : NULL;
    if (oc && oc->status == EMPYREAN_ORBIT_OUTCOME_FAILED) {
        char variant[256];
        propagate_failure_variant(variant, sizeof(variant), oc->error_code,
                                  oc->error_message);
        p->orbit_delivered = 0;
        char raw[512];
        snprintf(raw, sizeof(raw), "failed:%s", variant);
        sanitize_status(p->orbit_status, sizeof(p->orbit_status), raw);
    } else if (oc) {
        p->orbit_delivered = 1;
        if (attach && !have_cov) {
            sanitize_status(p->orbit_status, sizeof(p->orbit_status),
                            "cov_withheld:covariance_absent");
        } else {
            sanitize_status(p->orbit_status, sizeof(p->orbit_status), "delivered");
        }
    } else {
        p->orbit_delivered = 0;
        sanitize_status(p->orbit_status, sizeof(p->orbit_status), "no_outcome");
    }
    const struct EmpyreanPropagatedState* s = &res->states[0];
    if (s->has_mixture_tally) {
        p->has_mix = 1;
        p->mix_total = s->mixture_tally.n_components_total;
        p->mix_weight = s->mixture_tally.weight_delivered;
        p->mix_failed = s->mixture_tally.n_failed;
        p->mix_unresolved = s->mixture_tally.n_unresolved;
        p->mix_curvature = s->mixture_tally.n_curvature_refused;
        p->mix_sky = s->mixture_tally.n_sky_linearization_refused;
    }
}

/* Capture the 0.11 per-method products off a delivered ephemeris ENTRY — the
 * twin of `capture_prop_products` for the ephemeris seam. The row's sky
 * covariance is delivered as ONE packed joint (`entry->joint`, the per-row
 * covariance home beside the bare 6×6), so the kind / width / triangle read off
 * it through the same layout; the per-orbit outcome is `res->outcomes[0]`; the
 * six mixture tallies are the engine's PRE-RETENTION accounting
 * (`entry->has_mixture_tally` + `entry->mixture_tally`), unlike the propagation
 * row's retained-component tally. Copy the library-owned `tri` out BEFORE the
 * result is freed. `attach` names a withheld covariance rather than dropping it. */
static void capture_eph_products(struct Products* p,
                                 const struct EmpyreanEphemerisResult* res,
                                 const struct EmpyreanEphemerisEntry* entry,
                                 int attach) {
    memset(p, 0, sizeof(*p));
    const struct EmpyreanPackedJoint* j = &entry->joint;
    int have_cov = (j->width > 0 && j->tri != NULL);
    if (have_cov) {
        p->resolved_method = resolved_method_name(j->kind);
        p->has_cov_kind = 1;
        p->cov_kind = j->kind;
        p->has_cov_width = 1;
        p->cov_width = j->width;
        p->tri_len = (j->tri_len > 210) ? 210 : j->tri_len;
        for (uintptr_t i = 0; i < p->tri_len; i++) {
            p->tri[i] = j->tri[i];
        }
    }
    const struct EmpyreanOrbitOutcome* oc =
        (res->outcomes && res->num_orbits > 0) ? &res->outcomes[0] : NULL;
    if (oc && oc->status == EMPYREAN_ORBIT_OUTCOME_FAILED) {
        char variant[256];
        propagate_failure_variant(variant, sizeof(variant), oc->error_code,
                                  oc->error_message);
        p->orbit_delivered = 0;
        char raw[512];
        snprintf(raw, sizeof(raw), "failed:%s", variant);
        sanitize_status(p->orbit_status, sizeof(p->orbit_status), raw);
    } else if (oc) {
        p->orbit_delivered = 1;
        if (attach && !have_cov) {
            sanitize_status(p->orbit_status, sizeof(p->orbit_status),
                            "cov_withheld:covariance_absent");
        } else {
            sanitize_status(p->orbit_status, sizeof(p->orbit_status), "delivered");
        }
    } else {
        p->orbit_delivered = 0;
        sanitize_status(p->orbit_status, sizeof(p->orbit_status), "no_outcome");
    }
    if (entry->has_mixture_tally) {
        p->has_mix = 1;
        p->mix_total = entry->mixture_tally.n_components_total;
        p->mix_weight = entry->mixture_tally.weight_delivered;
        p->mix_failed = entry->mixture_tally.n_failed;
        p->mix_unresolved = entry->mixture_tally.n_unresolved;
        p->mix_curvature = entry->mixture_tally.n_curvature_refused;
        p->mix_sky = entry->mixture_tally.n_sky_linearization_refused;
    }
}

static int handle_prop(EmpyreanContext* ctx, const char* rest, int* warmed_up) {
    double epoch, x, y, z, vx, vy, vz;
    double a1, a2, a3, g[5];
    int force_model;
    double target;
    double non_grav_dt;
    /* Optional 19th token: the plan row's `propagation_uncertainty` composite
     * tag (every covariance-bearing method; `none` / the untagged legacy row
     * send the bare 18-field line, unchanged). `%63s` sets `parsed == 19` only
     * when a token is present. */
    char method[64] = {0};
    int parsed = sscanf(
        rest,
        "%lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %d %lf %lf %63s",
        &epoch, &x, &y, &z, &vx, &vy, &vz,
        &a1, &a2, &a3, &g[0], &g[1], &g[2], &g[3], &g[4],
        &force_model, &target, &non_grav_dt, method);
    if (parsed != 18 && parsed != 19) {
        printf("fail prop_parse_%d_fields\n", parsed);
        return 0;
    }
    int has_method = (parsed == 19);

    double pos[3] = {x, y, z}, vel[3] = {vx, vy, vz};
    struct EmpyreanOrbit orbit;
    build_orbit(&orbit, epoch, pos, vel, a1, a2, a3, g);
    orbit.non_grav_dt = non_grav_dt;
    struct EmpyreanPropagationConfig cfg;
    build_prop_config(&cfg, force_model);

    /* Per-method path: bind `uncertainty_method` and, for a covariance-bearing
     * method, attach the synthetic input covariance as the single packed joint.
     * `cov_tri_buf` must outlive the propagate call (the joint borrows it). */
    int attach = 0;
    double cov_tri_buf[21];
    if (has_method) {
        struct EmpyreanUncertaintyMethod um;
        if (!method_for_tag(method, &attach, &um)) {
            printf("fail unknown_uncertainty_method_%s\n", method);
            return 0;
        }
        cfg.uncertainty_method = um;
        if (attach) {
            attach_synthetic_joint(&orbit, cov_tri_buf);
        }
    }

    if (!*warmed_up) {
        for (int w = 0; w < 5; w++) {
            struct EmpyreanPropagationResult warm;
            memset(&warm, 0, sizeof(warm));
            empyrean_propagate(ctx, &orbit, 1, &target, 1, &cfg, &warm);
            empyrean_propagation_result_free(&warm);
        }
        *warmed_up = 1;
        fprintf(stderr, "warmup done\n");
        fflush(stderr);
    }

    struct EmpyreanPropagatedState last_state;
    memset(&last_state, 0, sizeof(last_state));
    struct Products prod;
    memset(&prod, 0, sizeof(prod));
    double best_ms = 1.0e308;
    int last_code = 0;
    const char* last_err = NULL;

    for (int rep = 0; rep < 3; rep++) {
        struct EmpyreanPropagationResult result;
        memset(&result, 0, sizeof(result));
        double t0 = now_ms();
        int code = empyrean_propagate(ctx, &orbit, 1, &target, 1, &cfg, &result);
        double ms = now_ms() - t0;
        if (code != 0 || result.num_states < 1) {
            last_code = code;
            last_err = empyrean_last_error();
            empyrean_propagation_result_free(&result);
            break;
        }
        if (ms < best_ms) best_ms = ms;
        last_state = result.states[0];
        /* Products are deterministic run-to-run; capture the last rep's
         * (copying the library-owned joint triangle) before the free. */
        if (has_method) {
            capture_prop_products(&prod, &result, attach);
        }
        empyrean_propagation_result_free(&result);
    }
    if (last_code != 0) {
        printf("fail %s\n", last_err ? last_err : "");
        return 0;
    }
    if (!has_method) {
        /* Covariance-free line — byte-identical to the pre-widening runner. */
        printf("ok %.18e %.18e %.18e %.18e %.18e %.18e %.6f\n",
               last_state.x, last_state.y, last_state.z,
               last_state.vx, last_state.vy, last_state.vz, best_ms);
        return 0;
    }
    printf("ok %.18e %.18e %.18e %.18e %.18e %.18e %.6f ",
           last_state.x, last_state.y, last_state.z,
           last_state.vx, last_state.vy, last_state.vz, best_ms);
    render_products(&prod);
    printf("\n");
    return 0;
}

static int handle_eph(EmpyreanContext* ctx, const char* rest) {
    double epoch, x, y, z, vx, vy, vz;
    double a1, a2, a3, g[5];
    int force_model;
    double target;
    double non_grav_dt;
    char obs_code[16] = {0};
    /* Optional 20th token: the plan row's `propagation_uncertainty` composite
     * tag. `%63s` sets `parsed == 20` only when present; a 19-field line (no
     * method) stays the covariance-free line, byte-identical to before. */
    char method[64] = {0};
    int parsed = sscanf(
        rest,
        "%lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %d %lf %lf %15s %63s",
        &epoch, &x, &y, &z, &vx, &vy, &vz,
        &a1, &a2, &a3, &g[0], &g[1], &g[2], &g[3], &g[4],
        &force_model, &target, &non_grav_dt, obs_code, method);
    if (parsed != 19 && parsed != 20) {
        printf("fail eph_parse_%d_fields\n", parsed);
        return 0;
    }
    int has_method = (parsed == 20);

    double pos[3] = {x, y, z}, vel[3] = {vx, vy, vz};
    struct EmpyreanOrbit orbit;
    build_orbit(&orbit, epoch, pos, vel, a1, a2, a3, g);
    orbit.non_grav_dt = non_grav_dt;
    struct EmpyreanEphemerisConfig cfg;
    memset(&cfg, 0, sizeof(cfg));
    build_prop_config(&cfg.propagation, force_model);
    /* Ephemeris generation uses EclipticJ2000 as the integration frame
     * (matches the wrapper's EphemerisConfig::with_force_model default).
     * The user-facing RA/Dec output is still in ICRF — only the
     * propagation phase is integrated in EclipticJ2000. */
    cfg.propagation.frame = 1; /* EclipticJ2000 */
    cfg.compute_diagnostics = 1;

    /* Per-method path: bind `uncertainty_method` and, for a covariance-bearing
     * method, attach the synthetic input covariance as the single packed joint.
     * `cov_tri_buf` must outlive the generate call (the joint borrows it). */
    int attach = 0;
    double cov_tri_buf[21];
    if (has_method) {
        struct EmpyreanUncertaintyMethod um;
        if (!method_for_tag(method, &attach, &um)) {
            printf("fail unknown_uncertainty_method_%s\n", method);
            return 0;
        }
        cfg.propagation.uncertainty_method = um;
        if (attach) {
            attach_synthetic_joint(&orbit, cov_tri_buf);
        }
    }

    /* Resolve observer at the target epoch via empyrean_get_observers. */
    const char* code_ptr = obs_code;
    struct EmpyreanObserverResult obs_result;
    memset(&obs_result, 0, sizeof(obs_result));
    /* Basis is explicit as of ABI 1000. (ICRF=0, SSB=0) is the construction
     * basis — the observers come back exactly as built, with no transform —
     * which is what ephemeris generation requires and what this call got
     * implicitly before. */
    int code = empyrean_get_observers(ctx, &code_ptr, 1, &target, 1,
                                      0 /* Frame::ICRF */, 0 /* Origin::SSB */,
                                      &obs_result);
    if (code != 0 || obs_result.num_observers < 1) {
        const char* err = empyrean_last_error();
        printf("fail get_observers: %s\n", err ? err : "no observer");
        empyrean_observer_result_free(&obs_result);
        return 0;
    }

    struct EmpyreanEphemerisResult result;
    double best_ms = 1.0e308;
    double last_ra = NAN, last_dec = NAN, last_rho = NAN, last_lt = NAN;
    int last_code = 0;
    const char* last_err = NULL;
    struct Products prod;
    memset(&prod, 0, sizeof(prod));

    for (int rep = 0; rep < 3; rep++) {
        memset(&result, 0, sizeof(result));
        double t0 = now_ms();
        int rc = empyrean_generate_ephemeris(ctx, &orbit, 1,
                                              obs_result.observers, 1,
                                              &cfg, &result);
        double ms = now_ms() - t0;
        if (rc != 0 || result.num_entries < 1) {
            last_code = rc;
            last_err = empyrean_last_error();
            empyrean_ephemeris_result_free(&result);
            break;
        }
        if (ms < best_ms) best_ms = ms;
        last_ra = result.entries[0].ra_deg;
        last_dec = result.entries[0].dec_deg;
        last_rho = result.entries[0].rho_au;
        last_lt = result.entries[0].light_time_days;
        /* Products are deterministic run-to-run; capture the last rep's off the
         * delivered entry (copying the library-owned joint triangle) before the
         * free. */
        if (has_method) {
            capture_eph_products(&prod, &result, &result.entries[0], attach);
        }
        empyrean_ephemeris_result_free(&result);
    }
    empyrean_observer_result_free(&obs_result);

    if (last_code != 0) {
        printf("fail %s\n", last_err ? last_err : "");
        return 0;
    }
    if (!has_method) {
        /* Covariance-free line — byte-identical to the pre-widening runner. */
        printf("ok %.18e %.18e %.18e %.18e %.6f\n",
               last_ra, last_dec, last_rho, last_lt, best_ms);
        return 0;
    }
    printf("ok %.18e %.18e %.18e %.18e %.6f ",
           last_ra, last_dec, last_rho, last_lt, best_ms);
    render_products(&prod);
    printf("\n");
    return 0;
}

/* Shared OD driver for every mode. `solve_non_grav` flips solve_for from Auto
 * to StateAndNonGrav and switches the emitted line to the fitted A1/A2/A3 + σ
 * surface (the `odng` non-grav-recovery second pass). `method_tag` is the plan
 * row's composite `<method>_<arm>` tag for a per-method fit, or NULL for the
 * legacy untagged fit (and the non-grav pass); a non-NULL tag binds
 * `ODConfig.uncertainty_method` and switches the line to the per-method
 * products (delivered) or a `refused <engine text>` line (a method the engine
 * refuses by name). Everything else — config, fixture read, perturber
 * exclusion — is identical so a per-method fit differs from the legacy fit by
 * `uncertainty_method` alone (config parity). */
static int run_od(EmpyreanContext* ctx, int force_model, int exclude_naif,
                  const char* ades_path, int solve_non_grav,
                  const char* method_tag) {
    /* Resolve the per-method rung first: an unrecognized tag is refused by name
     * before any work (the no-silent-substitution invariant). */
    int attach = 0;
    struct EmpyreanUncertaintyMethod um;
    memset(&um, 0, sizeof(um)); /* tag 0 = FIRST: the legacy / odng default */
    if (method_tag) {
        if (!method_for_tag(method_tag, &attach, &um)) {
            printf("fail unknown_uncertainty_method_%s\n", method_tag);
            return 0;
        }
    }

    /* Read PSV file content. */
    FILE* f = fopen(ades_path, "r");
    if (!f) {
        printf("fail od_open_%s\n", ades_path);
        return 0;
    }
    fseek(f, 0, SEEK_END);
    long len = ftell(f);
    fseek(f, 0, SEEK_SET);
    char* content = (char*)malloc((size_t)len + 1);
    if (!content) {
        fclose(f);
        printf("fail od_oom\n");
        return 0;
    }
    size_t n = fread(content, 1, (size_t)len, f);
    content[n] = '\0';
    fclose(f);

    struct EmpyreanObservation* observations = NULL;
    uintptr_t num_observations = 0;
    /* These OD fixtures are optical-only PSVs; the radar out-params are
     * present in the current read_ades signature (the C ABI carries radar
     * astrometry through too) but come back empty here. Capture + free them
     * so we don't leak if a future fixture grows a `<radar>` table. */
    struct EmpyreanRadarObservation* radar = NULL;
    uintptr_t num_radar = 0;
    int rc = empyrean_read_ades(content, &observations, &num_observations,
                                &radar, &num_radar);
    free(content);
    if (rc != 0) {
        const char* err = empyrean_last_error();
        printf("fail read_ades: %s\n", err ? err : "");
        empyrean_radar_observations_free(radar, num_radar);
        return 0;
    }

    struct EmpyreanODConfig cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.force_model = force_model;
    /* Per-row fork-exec runner — pin to 1 thread (see build_prop_config
     * comment) so a parallel driver doesn't blow ulimit -u. */
    cfg.num_threads = 1;

    /* Production OD defaults — must mirror the engine's
     * ODConfig::default() field-for-field so the c channel's fits agree
     * with the core channel (validate-core, which calls it directly). Several
     * EmpyreanODConfig fields are read unconditionally on the FFI side,
     * so zero-init silently sets them to non-production values:
     *
     *   - weighting.enabled = 0 → uniform 1″ (engine default: VFCC2017)
     *   - debiasing.enabled = 0 → no catalog debiasing (engine: EFCC2020)
     *   - allow_arc_truncation = 0 → truncation FORBIDDEN (engine: allowed)
     *   - coorbital_enabled = 0 → co-orbital IOD lane OFF (engine: enabled)
     *   - solve_for = 0 → STATE_ONLY (engine default: Auto)
     *   - rejection.enabled = 0 → no outlier rejection (engine: Adaptive)
     *   - rejection.lambda = 0 → 0.0 information weight (engine: 1.0;
     *     `-1.0` is the "use upstream default" sentinel)
     *
     * Each one would silently move the c channel's fit away from the
     * core baseline. solve_for=STATE_ONLY is especially dangerous on
     * comets (67P / 2I/Borisov / etc.) because non-grav coefficients
     * are NOT fit, leaving thousands of km of unmodeled radial drift. */
    cfg.weighting.enabled = 1;
    /* Renamed from EMPYREAN_WEIGHTING_PRESET_VFC17 at ABI 1000; same
     * value (1), same scheme — Vereš, Farnocchia, Chesley et al. (2017). */
    cfg.weighting.preset = EMPYREAN_WEIGHTING_PRESET_VFCC2017;
    cfg.weighting.sigma_policy = -1; /* use preset's policy */
    struct EmpyreanWeightingLayer nightly;
    memset(&nightly, 0, sizeof(nightly));
    nightly.kind = EMPYREAN_WEIGHTING_LAYER_NIGHTLY_DEWEIGHTING;
    nightly.max_gap_days = 0.5;
    cfg.weighting.additional_layers = &nightly;
    cfg.weighting.num_additional_layers = 1;

    cfg.debiasing.enabled = 1;
    cfg.debiasing.table_id = EMPYREAN_DEBIASING_TABLE_EFCC2020;
    cfg.debiasing.resolution = EMPYREAN_DEBIASING_RESOLUTION_STANDARD;
    cfg.debiasing.bias_dat_path = NULL; /* DataManager default location */

    /* `use_stm_cache` is gone at ABI 1000 — it was a control that did
     * nothing engine-side, and its slot is now these two axes. Both are
     * tri-state with NEGATIVE meaning "engine default", so the memset(0)
     * above does NOT leave them defaulted: it reads as truncation
     * FORBIDDEN and the co-orbital IOD lane FORCED OFF. Set both to -1 so
     * this channel fits under the same policy as the core reference. */
    cfg.allow_arc_truncation = -1;
    cfg.coorbital_enabled = -1;
    /* Optical-only OD leaves solve_for = Auto (the engine default); the
     * non-grav-recovery pass explicitly forces StateAndNonGrav so the fit
     * solves the full (state, A1, A2, A3) parameter set and populates the
     * 9×9 covariance whose diagonal carries σ_A1/σ_A2/σ_A3. */
    cfg.solve_for =
        solve_non_grav ? EMPYREAN_SOLVE_FOR_STATE_AND_NONGRAV : EMPYREAN_SOLVE_FOR_AUTO;

    cfg.rejection.enabled = 1;
    cfg.rejection.kind = EMPYREAN_REJECTION_KIND_ADAPTIVE;
    cfg.rejection.lambda = -1.0; /* sentinel: use the engine default (1.0) */

    /* The per-method uncertainty axis. `memset(0)` above left
     * `uncertainty_method.tag = FIRST` — the historical default the legacy /
     * non-grav fits keep (byte-identical). A method-tagged fit overrides it
     * with the tag's rung; the fit is first-order by construction, so FirstOrder
     * and Auto deliver and every richer method is refused by the engine by name
     * (handled at the delivery check below). */
    if (method_tag) {
        cfg.uncertainty_method = um;
    }

    /* Self-perturber exclusion (SB441-N16 bodies that would otherwise
     * pull on themselves through the perturber set). The driver passes
     * exclude_naif=0 when no exclusion applies. */
    int32_t excluded_naif_storage = exclude_naif;
    if (exclude_naif != 0) {
        cfg.num_excluded_perturbers = 1;
        cfg.excluded_perturbers_naif = &excluded_naif_storage;
    }
    /* Remaining zero-init fields are guarded on the FFI side with
     * "use engine default if 0/null" sentinels: max_iterations,
     * convergence_tol, epsilon, max_light_time_iterations,
     * output_epoch.mode (0 = MidArc, matches the engine),
     * acceptability/auto_escalation thresholds (0 = use the engine),
     * rejection.chi2_base (0 = use 9.21), rejection.max_threshold
     * (0 = use 100.0). */

    /* determine is batch-first: it groups the observations by ADES object
     * identifier and returns one slot per group. These fixtures are one
     * object each, so the batch must hold exactly one delivered slot —
     * anything else is a fixture or engine problem this row must report,
     * never paper over by picking a slot. */
    struct EmpyreanDetermineResults batch;
    memset(&batch, 0, sizeof(batch));
    double t0 = now_ms();
    /* radar = NULL, 0 (optical-only fixtures) and no DC seed orbits
     * (NULL, 0 → let the IOD pipeline produce its own seeds). */
    rc = empyrean_determine(ctx, observations, num_observations,
                             radar, num_radar, NULL, 0, &cfg, &batch);
    double ms = now_ms() - t0;

    empyrean_observations_free(observations, num_observations);
    empyrean_radar_observations_free(radar, num_radar);

    /* 0 = at least one object delivered; -4 = the batch ran and every
     * object failed. Both populate the table and both must be freed. Any
     * other code left it untouched. */
    if (rc != 0 && rc != EMPYREAN_DETERMINE_NONE_DELIVERED) {
        const char* err = empyrean_last_error();
        printf("fail determine: %s\n", err ? err : "");
        empyrean_determine_results_free(&batch);
        return 0;
    }
    if (batch.num_objects != 1) {
        /* Zero groups (no identifiable rows) or several (a fixture carrying
         * more than one object). Naming the count keeps this from reading as
         * a fit failure, which it is not. */
        printf("fail determine_grouped_%zu_objects_expected_1\n",
               (size_t)batch.num_objects);
        empyrean_determine_results_free(&batch);
        return 0;
    }
    const struct EmpyreanODObjectResult* slot = &batch.objects[0];
    if (!slot->delivered) {
        /* This object's fit failed. `slot->result` is NaN-poisoned, so
         * reading it would emit plausible-looking garbage; report the
         * engine's own message instead. For a method-tagged fit the engine
         * refuses a richer method BY NAME (its text names the method); emit
         * that as a `refused <text>` line (trailing, so spaces are preserved)
         * the driver maps to `orbit_delivered = false` — never a silent
         * downgrade. */
        if (method_tag) {
            printf("refused %s\n", slot->error ? slot->error : "");
        } else {
            printf("fail determine: %s\n", slot->error ? slot->error : "");
        }
        empyrean_determine_results_free(&batch);
        return 0;
    }
    const struct EmpyreanODResult result = slot->result;

    if (method_tag) {
        /* Per-method delivered fit: `ok` + state[6] + iters + ms + the 12
         * per-method product tokens. The fit is first-order by construction, so
         * `resolved_method` is the delivered kind (linear) and the joint is the
         * fitted state's packed joint (`orbit.orbit_cov`); `none` publishes the
         * fit covariance-free (attach == 0). An OD fit is never a mixture, so
         * the mix tallies are always absent. Mirrors the cli binary's
         * `Products::from_od`. */
        struct Products p;
        memset(&p, 0, sizeof(p));
        if (attach) {
            p.resolved_method = resolved_method_name(result.resolved_method);
            const struct EmpyreanPackedJoint* j = &result.orbit.orbit_cov;
            if (j->width > 0 && j->tri != NULL) {
                p.has_cov_kind = 1;
                p.cov_kind = j->kind;
                p.has_cov_width = 1;
                p.cov_width = j->width;
                p.tri_len = (j->tri_len > 210) ? 210 : j->tri_len;
                for (uintptr_t i = 0; i < p.tri_len; i++) {
                    p.tri[i] = j->tri[i];
                }
            }
        }
        p.orbit_delivered = 1;
        sanitize_status(p.orbit_status, sizeof(p.orbit_status), "delivered");
        p.has_mix = 0;
        printf("ok %.18e %.18e %.18e %.18e %.18e %.18e %u %.6f ",
               result.orbit.x, result.orbit.y, result.orbit.z,
               result.orbit.vx, result.orbit.vy, result.orbit.vz,
               (unsigned)result.iterations, ms);
        render_products(&p);
        printf("\n");
    } else if (solve_non_grav) {
        /* Non-grav recovery: emit the fitted A1/A2/A3 (AU/day²) and their 1σ
         * from the 9×9 covariance diagonal — σ_Ai = sqrt(C9x9[6+i][6+i]).
         *
         * Loud-failure guard: if non-grav was NOT actually recovered — i.e.
         * the 9×9 covariance is absent (has_covariance_9x9 == 0; the fit
         * silently fell back to a 6-param state-only solve) or the fitted
         * value/variance is non-finite — emit the literal token "null" for
         * BOTH the coefficient and its σ so a missing value reads as
         * "non-grav not recovered" (never 0, never NaN). The driver maps
         * "null" to JSON null on the od_a1/od_a2/od_a3 and their _sigma
         * fields. */
        int have_ng = result.has_non_grav && result.has_covariance_9x9;
        double a[3] = {result.non_grav.a1, result.non_grav.a2, result.non_grav.a3};
        double var[3] = {
            result.covariance_9x9[6][6],
            result.covariance_9x9[7][7],
            result.covariance_9x9[8][8],
        };
        char a_str[3][32];
        char sig_str[3][32];
        for (int i = 0; i < 3; i++) {
            if (have_ng && isfinite(a[i]) && isfinite(var[i]) && var[i] >= 0.0) {
                snprintf(a_str[i], sizeof(a_str[i]), "%.18e", a[i]);
                snprintf(sig_str[i], sizeof(sig_str[i]), "%.18e", sqrt(var[i]));
            } else {
                snprintf(a_str[i], sizeof(a_str[i]), "null");
                snprintf(sig_str[i], sizeof(sig_str[i]), "null");
            }
        }
        /* Trailing rms_combined + fitted heliocentric position (x,y,z) of
         * the non-grav solution — the driver writes them to the
         * non_grav_recovery row's od_rms_combined_arcsec / emp_pos_au so it
         * reports the NON-GRAV fit's residual and state, not the optical
         * one. Always finite (the state-only fall-back still has both). */
        printf("ok %s %s %s %s %s %s %u %.6f %.18e %.18e %.18e %.18e\n",
               a_str[0], a_str[1], a_str[2],
               sig_str[0], sig_str[1], sig_str[2],
               (unsigned)result.iterations, ms,
               result.summary.rms_combined_arcsec,
               result.orbit.x, result.orbit.y, result.orbit.z);
    } else {
        /* Trailing rms_combined so the driver sets the orbit_determination
         * row's od_rms_combined_arcsec from the c channel's own fit rather
         * than inheriting the (now-stripped) plan value. */
        printf("ok %.18e %.18e %.18e %.18e %.18e %.18e %u %.6f %.18e\n",
               result.orbit.x, result.orbit.y, result.orbit.z,
               result.orbit.vx, result.orbit.vy, result.orbit.vz,
               (unsigned)result.iterations, ms,
               result.summary.rms_combined_arcsec);
    }
    /* `result` is a copy of the slot's fit; the storage belongs to the
     * batch table and is released with it. */
    empyrean_determine_results_free(&batch);
    return 0;
}

/* Copy the trailing path token (everything after the parsed prefix) into `out`,
 * stripping the trailing newline. PSV fixture filenames can contain spaces
 * (e.g. "2020 XL5.psv"), so the path is the whole remainder, never a `%s`
 * field. Returns 1 on success, 0 (and prints a fail line) otherwise. */
static int copy_od_path(const char* path_start, char* out, size_t outsz) {
    size_t path_len = strlen(path_start);
    while (path_len > 0
           && (path_start[path_len - 1] == '\n' || path_start[path_len - 1] == '\r')) {
        path_len--;
    }
    if (path_len == 0 || path_len >= outsz) {
        printf("fail od_path_invalid\n");
        return 0;
    }
    memcpy(out, path_start, path_len);
    out[path_len] = '\0';
    return 1;
}

/* `od FORCE EXCLUDE_NAIF METHOD PATH` — the optical fit, now method-aware.
 * METHOD is `-` for the legacy untagged fit (the 10-field line, byte-identical
 * to before) or a composite `<method>_<arm>` tag for a per-method fit.
 * EXCLUDE_NAIF = 0 means no perturber exclusion. */
static int handle_od(EmpyreanContext* ctx, const char* rest) {
    int force_model, exclude_naif, n_consumed = 0;
    char method[64] = {0};
    int parsed =
        sscanf(rest, "%d %d %63s %n", &force_model, &exclude_naif, method, &n_consumed);
    if (parsed != 3) {
        printf("fail od_parse_force\n");
        return 0;
    }
    char ades_path[1024];
    if (!copy_od_path(rest + n_consumed, ades_path, sizeof(ades_path))) {
        return 0;
    }
    const char* method_tag = (strcmp(method, "-") == 0) ? NULL : method;
    return run_od(ctx, force_model, exclude_naif, ades_path, 0, method_tag);
}

/* `odng FORCE EXCLUDE_NAIF PATH` — the non-grav-recovery second pass. Always
 * the legacy untagged fit (no method axis on this seam; it rides the untagged
 * fit once per object, mirroring the rust / cli channels). */
static int handle_odng(EmpyreanContext* ctx, const char* rest) {
    int force_model, exclude_naif, n_consumed = 0;
    int parsed = sscanf(rest, "%d %d %n", &force_model, &exclude_naif, &n_consumed);
    if (parsed != 2) {
        printf("fail od_parse_force\n");
        return 0;
    }
    char ades_path[1024];
    if (!copy_od_path(rest + n_consumed, ades_path, sizeof(ades_path))) {
        return 0;
    }
    return run_od(ctx, force_model, exclude_naif, ades_path, 1, NULL);
}

int main(int argc, char** argv) {
    const char* data_dir = (argc >= 2) ? argv[1] : NULL;

    /* ABI handshake, before the first call that reads a struct.
     *
     * This runner is compiled against `empyrean.h` and linked against
     * whichever `libempyrean` DYLD_LIBRARY_PATH resolves to at run time —
     * two artifacts that can disagree. `EMPYREAN_ABI_VERSION` is the
     * header's compile-time constant and `empyrean_abi_version()` reads the
     * loaded library's, and the contract is equality: the versions encode
     * struct layout, so a mismatch is not a degraded run but a
     * reinterpretation of every struct this file passes across the
     * boundary. Left unchecked it surfaces as a segfault at best and as
     * plausible wrong numbers at worst — a validation channel silently
     * measuring a different library than the one it claims.
     *
     * Checked here rather than per row: it cannot change mid-process. */
    uint32_t loaded_abi = empyrean_abi_version();
    if (loaded_abi != EMPYREAN_ABI_VERSION) {
        fprintf(stderr,
                "fatal: empyrean ABI mismatch — runner compiled against "
                "version %u, loaded libempyrean reports %u.\n"
                "       The header and the library come from different "
                "releases; struct layouts do not match and no result from "
                "this process would be trustworthy.\n"
                "       Rebuild libempyrean from the empyrean checkout this "
                "harness validates (see EMPYREAN_ROOT in the Makefile) and "
                "re-run `make build-empyrean-c build-c`.\n",
                (unsigned)EMPYREAN_ABI_VERSION, (unsigned)loaded_abi);
        return 1;
    }

    EmpyreanContext* ctx = empyrean_context_from_data_dir(data_dir);
    if (!ctx) {
        fprintf(stderr, "fatal: empyrean_context_from_data_dir failed: %s\n",
                empyrean_last_error());
        return 1;
    }
    fprintf(stderr, "ready\n");
    fflush(stderr);

    char line[8192];
    int warmed_up = 0;
    while (fgets(line, sizeof(line), stdin)) {
        /* First word: mode. Strip from buffer; rest is per-mode args. */
        char* p = line;
        while (*p == ' ' || *p == '\t') p++;
        char mode[8] = {0};
        int mi = 0;
        while (*p && *p != ' ' && *p != '\t' && *p != '\n' && mi < 7) {
            mode[mi++] = *p++;
        }
        mode[mi] = '\0';
        while (*p == ' ' || *p == '\t') p++;
        const char* rest = p;

        if (strcmp(mode, "prop") == 0) {
            handle_prop(ctx, rest, &warmed_up);
        } else if (strcmp(mode, "eph") == 0) {
            handle_eph(ctx, rest);
        } else if (strcmp(mode, "od") == 0) {
            handle_od(ctx, rest);
        } else if (strcmp(mode, "odng") == 0) {
            handle_odng(ctx, rest);
        } else {
            printf("fail unknown_mode_%s\n", mode);
        }
        fflush(stdout);
    }

    empyrean_context_free(ctx);
    return 0;
}
