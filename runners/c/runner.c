/* empyrean validation runner — C channel.
 *
 * Tests the public C API directly (libempyrean.dylib + empyrean.h).
 * Reads space-separated command lines from stdin and writes results
 * to stdout. Driven by validation/runners/c/drive.py.
 *
 * Three modes, dispatched on the first whitespace-separated token:
 *
 *   prop EPOCH IC[6] A1 A2 A3 G[5] FORCE TARGET
 *     → ok x y z vx vy vz time_ms
 *
 *   eph EPOCH IC[6] A1 A2 A3 G[5] FORCE TARGET OBS_CODE
 *     → ok ra_deg dec_deg rho_au lt_d time_ms
 *
 *   od FORCE EXCLUDE_NAIF ADES_PATH    (path is last; may contain spaces;
 *                                       EXCLUDE_NAIF=0 → no exclusion)
 *     → ok x y z vx vy vz iterations time_ms
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

static int handle_prop(EmpyreanContext* ctx, const char* rest, int* warmed_up) {
    double epoch, x, y, z, vx, vy, vz;
    double a1, a2, a3, g[5];
    int force_model;
    double target;
    double non_grav_dt;
    int parsed = sscanf(
        rest,
        "%lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %d %lf %lf",
        &epoch, &x, &y, &z, &vx, &vy, &vz,
        &a1, &a2, &a3, &g[0], &g[1], &g[2], &g[3], &g[4],
        &force_model, &target, &non_grav_dt);
    if (parsed != 18) {
        printf("fail prop_parse_%d_fields\n", parsed);
        return 0;
    }

    double pos[3] = {x, y, z}, vel[3] = {vx, vy, vz};
    struct EmpyreanOrbit orbit;
    build_orbit(&orbit, epoch, pos, vel, a1, a2, a3, g);
    orbit.non_grav_dt = non_grav_dt;
    struct EmpyreanPropagationConfig cfg;
    build_prop_config(&cfg, force_model);

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
        empyrean_propagation_result_free(&result);
    }
    if (last_code != 0) {
        printf("fail %s\n", last_err ? last_err : "");
        return 0;
    }
    printf("ok %.18e %.18e %.18e %.18e %.18e %.18e %.6f\n",
           last_state.x, last_state.y, last_state.z,
           last_state.vx, last_state.vy, last_state.vz, best_ms);
    return 0;
}

static int handle_eph(EmpyreanContext* ctx, const char* rest) {
    double epoch, x, y, z, vx, vy, vz;
    double a1, a2, a3, g[5];
    int force_model;
    double target;
    double non_grav_dt;
    char obs_code[16] = {0};
    int parsed = sscanf(
        rest,
        "%lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %lf %d %lf %lf %15s",
        &epoch, &x, &y, &z, &vx, &vy, &vz,
        &a1, &a2, &a3, &g[0], &g[1], &g[2], &g[3], &g[4],
        &force_model, &target, &non_grav_dt, obs_code);
    if (parsed != 19) {
        printf("fail eph_parse_%d_fields\n", parsed);
        return 0;
    }

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

    /* Resolve observer at the target epoch via empyrean_get_observers. */
    const char* code_ptr = obs_code;
    struct EmpyreanObserverResult obs_result;
    memset(&obs_result, 0, sizeof(obs_result));
    int code = empyrean_get_observers(ctx, &code_ptr, 1, &target, 1, &obs_result);
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
        empyrean_ephemeris_result_free(&result);
    }
    empyrean_observer_result_free(&obs_result);

    if (last_code != 0) {
        printf("fail %s\n", last_err ? last_err : "");
        return 0;
    }
    printf("ok %.18e %.18e %.18e %.18e %.6f\n",
           last_ra, last_dec, last_rho, last_lt, best_ms);
    return 0;
}

static int handle_od(EmpyreanContext* ctx, const char* rest) {
    int force_model;
    int exclude_naif;
    char ades_path[1024];
    /* FORCE EXCLUDE_NAIF come first as ints; the rest of the line (after
     * the second whitespace) is the path. PSV fixture filenames can
     * contain spaces (e.g. "2020 XL5.psv"), so sscanf %s won't work for
     * the path. exclude_naif=0 → no perturber exclusion. */
    int n_consumed = 0;
    int parsed = sscanf(rest, "%d %d %n", &force_model, &exclude_naif, &n_consumed);
    if (parsed != 2) {
        printf("fail od_parse_force\n");
        return 0;
    }
    const char* path_start = rest + n_consumed;
    /* Strip trailing newline. */
    size_t path_len = strlen(path_start);
    while (path_len > 0
           && (path_start[path_len - 1] == '\n' || path_start[path_len - 1] == '\r')) {
        path_len--;
    }
    if (path_len == 0 || path_len >= sizeof(ades_path)) {
        printf("fail od_path_invalid\n");
        return 0;
    }
    memcpy(ades_path, path_start, path_len);
    ades_path[path_len] = '\0';

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
    int rc = empyrean_read_ades(content, &observations, &num_observations);
    free(content);
    if (rc != 0) {
        const char* err = empyrean_last_error();
        printf("fail read_ades: %s\n", err ? err : "");
        return 0;
    }

    struct EmpyreanODConfig cfg;
    memset(&cfg, 0, sizeof(cfg));
    cfg.force_model = force_model;
    /* Per-row fork-exec runner — pin to 1 thread (see build_prop_config
     * comment) so a parallel driver doesn't blow ulimit -u. */
    cfg.num_threads = 1;

    /* Production OD defaults — must mirror scott::od::ODConfig::default()
     * field-for-field so the c channel's fits agree with the core
     * channel (validate-core, which calls scott directly). Several
     * EmpyreanODConfig fields are read unconditionally on the FFI side,
     * so zero-init silently sets them to non-production values:
     *
     *   - weighting.enabled = 0 → uniform 1″ (scott default: VFC17)
     *   - debiasing.enabled = 0 → no catalog debiasing (scott: EFCC2020)
     *   - use_stm_cache = 0 → STM cache off (scott default: on)
     *   - solve_for = 0 → STATE_ONLY (scott default: Auto)
     *   - rejection.enabled = 0 → no outlier rejection (scott: Adaptive)
     *   - rejection.lambda = 0 → 0.0 information weight (scott: 1.0;
     *     `-1.0` is the "use upstream default" sentinel)
     *
     * Each one would silently move the c channel's fit away from the
     * core baseline. solve_for=STATE_ONLY is especially dangerous on
     * comets (67P / 2I/Borisov / etc.) because non-grav coefficients
     * are NOT fit, leaving thousands of km of unmodeled radial drift. */
    cfg.weighting.enabled = 1;
    cfg.weighting.preset = EMPYREAN_WEIGHTING_PRESET_VFC17;
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

    cfg.use_stm_cache = 1;
    cfg.solve_for = EMPYREAN_SOLVE_FOR_AUTO;

    cfg.rejection.enabled = 1;
    cfg.rejection.kind = EMPYREAN_REJECTION_KIND_ADAPTIVE;
    cfg.rejection.lambda = -1.0; /* sentinel: use scott's default (1.0) */

    /* Self-perturber exclusion (SB441-N16 bodies that would otherwise
     * pull on themselves through the perturber set). The driver passes
     * exclude_naif=0 when no exclusion applies. */
    int32_t excluded_naif_storage = exclude_naif;
    if (exclude_naif != 0) {
        cfg.num_excluded_perturbers = 1;
        cfg.excluded_perturbers_naif = &excluded_naif_storage;
    }
    /* Remaining zero-init fields are guarded on the FFI side with
     * "use scott default if 0/null" sentinels: max_iterations,
     * convergence_tol, epsilon, max_light_time_iterations,
     * output_epoch.mode (0 = MidArc, matches scott),
     * acceptability/auto_escalation thresholds (0 = use scott),
     * rejection.chi2_base (0 = use 9.21), rejection.max_threshold
     * (0 = use 100.0). */

    struct EmpyreanODResult result;
    memset(&result, 0, sizeof(result));
    double t0 = now_ms();
    rc = empyrean_determine(ctx, observations, num_observations,
                             NULL, 0, &cfg, &result);
    double ms = now_ms() - t0;

    empyrean_observations_free(observations, num_observations);

    if (rc != 0) {
        const char* err = empyrean_last_error();
        printf("fail determine: %s\n", err ? err : "");
        empyrean_od_result_free(&result);
        return 0;
    }

    printf("ok %.18e %.18e %.18e %.18e %.18e %.18e %u %.6f\n",
           result.orbit.x, result.orbit.y, result.orbit.z,
           result.orbit.vx, result.orbit.vy, result.orbit.vz,
           (unsigned)result.iterations, ms);
    empyrean_od_result_free(&result);
    return 0;
}

int main(int argc, char** argv) {
    const char* data_dir = (argc >= 2) ? argv[1] : NULL;

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
        } else {
            printf("fail unknown_mode_%s\n", mode);
        }
        fflush(stdout);
    }

    empyrean_context_free(ctx);
    return 0;
}
