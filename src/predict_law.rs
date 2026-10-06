//! Law-aware scoring of a covariance-realism residual.
//!
//! A held-out sky residual \\( r \in \mathbb{R}^2 \\) is scored under the
//! predictive law the producing arm assumed. Two laws are supported, both
//! carrying a scoring observation covariance \\( \Sigma_{obs} \\) (the pinned
//! table — arm-independent, a Gaussian-*equivalent* covariance, i.e. the
//! variance) and the arm's predicted sky covariance \\( \Sigma_{pred} \\).
//! Write \\( C = \Sigma_{obs} + \Sigma_{pred} \\) and
//! \\( d^2 = r^\top C^{-1} r \\).
//!
//! # Normal law
//!
//! \\( r \sim \mathcal N(0, C) \\), so the log predictive density is
//! \\[ \ln p(r) = -\tfrac12\left(d^2 + \ln\det C + 2\ln 2\pi\right), \\]
//! and the reference CDF of \\( d^2 \\) is that of \\( \chi^2_2 \\),
//! \\( F(c) = 1 - e^{-c/2} \\).
//!
//! # Joint bivariate Student-t law (\\( \nu > 2 \\), variance convention)
//!
//! The observation error is a scale mixture,
//! \\( e \mid w \sim \mathcal N(0, s(w)\,\Sigma_{obs}) \\) with
//! \\( s(w) = (\nu-2)/(\nu w) \\) and \\( w \sim \chi^2_\nu/\nu \\)
//! (equivalently \\( w \sim \mathrm{Gamma}(\tfrac\nu2, \tfrac\nu2) \\), shape
//! and rate), so \\( \mathbb E[s] = 1 \\) and \\( \operatorname{Var}(e) =
//! \Sigma_{obs} \\); the prediction error is
//! \\( \mathcal N(0, \Sigma_{pred}) \\), independent. Hence
//! \\( r \mid w \sim \mathcal N(0, A_w) \\),
//! \\( A_w = \Sigma_{pred} + s(w)\,\Sigma_{obs} \\), and
//! \\[ p(r) = \int_0^\infty \mathcal N(r; 0, A_w)\, g(w)\, dw, \\]
//! a one-dimensional integral against the Gamma density \\( g \\). The
//! reference CDF of \\( d^2 = r^\top C^{-1} r \\) (note: \\( C \\) fixed) is,
//! with \\( \lambda_1(w), \lambda_2(w) \\) the eigenvalues of
//! \\( C^{-1/2} A_w C^{-1/2} \\) and
//! \\( q(\theta) = \lambda_1\cos^2\theta + \lambda_2\sin^2\theta \\),
//! \\[ P(d^2 \le c \mid w) = \frac1{2\pi}\int_0^{2\pi}
//!     \left[1 - \exp\!\left(-\frac{c}{2\,q(\theta)}\right)\right] d\theta,
//!    \qquad
//!    F(c) = \int_0^\infty P(d^2 \le c \mid w)\, g(w)\, dw. \\]
//!
//! # PIT and the proper scoring rule
//!
//! The probability integral transform of a row is \\( u = F(d^2) \\) under the
//! row's reference law. If the covariance is calibrated then
//! \\( u \sim \mathrm{Uniform}(0,1) \\) whatever the law and whatever the
//! \\( \Sigma_{pred}/\Sigma_{obs} \\) mix, so rows pool across cells. The log
//! predictive density \\( \ln p(r) \\) is a strictly proper scoring rule: its
//! expectation is maximized by the data-generating law, so the mean log score
//! ranks competing arms and error models.
//!
//! # Limiting cases (all tested)
//!
//! - \\( \Sigma_{pred}\to 0 \\):
//!   \\( F(c) = 1 - (1 + c/(\nu-2))^{-\nu/2} \\) and the joint elliptical-t
//!   density.
//! - \\( \Sigma_{obs}\to 0 \\): the \\( \chi^2_2 \\) / normal results.
//! - \\( \nu\to\infty \\): the normal results.
//! - \\( \Sigma_{pred}\propto\Sigma_{obs} \\): \\( \lambda_1=\lambda_2 \\), so
//!   the \\( \theta \\) integral collapses.
//!
//! # Quadrature
//!
//! With \\( w \sim \mathrm{Gamma}(a, b) \\), \\( a = b = \nu/2 \\), the
//! \\( w \\) integral \\( \int_0^\infty F(w)\, g(w)\, dw \\) is taken in the
//! log variable \\( \xi = \ln w \\), where the integrand \\( F(w)\,g(w)\,w \\)
//! is a smooth bump that decays to zero at both ends (as \\( w^a \\) at
//! \\( w\to0 \\) and \\( e^{-bw} \\) at \\( w\to\infty \\)). The trapezoidal
//! rule is spectrally accurate for exactly such a doubly-decaying integrand.
//! The [`W_NODES`] nodes span a Gamma-quantile bracket
//! \\( [Q(10^{-13}), Q(1-10^{-13})] \\), so the mesh **auto-adapts** to each
//! \\( \nu \\): a heavy-tailed small-\\( \nu \\) bump gets a wide bracket, a
//! peaked large-\\( \nu \\) bump a narrow one, at the same nodes-per-width.
//! A generalized Gauss–Laguerre rule was rejected: its algebraic convergence
//! at small \\( \nu \\) (heavy tail) would need thousands of nodes to reach
//! \\( 10^{-6} \\). Nodes and per-node weights depend only on \\( \nu \\) and
//! are built once per law. The \\( \theta \\) integral is a smooth periodic
//! integrand (period \\( \pi \\)) scored by a midpoint rule whose node count
//! adapts to the eigenvalue ratio (a sharper integrand for a more anisotropic
//! covariance gets more nodes), with the isotropic case in closed form.
//! Measured accuracy is verified in the tests against an independent
//! composite-Simpson integration in \\( w \\) over the stated ranges
//! (\\( \nu\in[2.5,100] \\), trace ratio \\( \in[10^{-4},10^{4}] \\), axis
//! ratios up to 100).
//!
//! The density is accumulated in log space (log-sum-exp) so an extreme
//! residual yields a finite, very negative log score rather than an underflow.

use hyperjet::statistics::ln_gamma;

/// A symmetric 2×2 matrix in row-major order (RA-ish, Dec).
pub type Mat2 = [[f64; 2]; 2];

/// The three squared-Mahalanobis thresholds coverage is reported at
/// (\\( d^2 \le 1, 4, 9 \\) — the 1σ, 2σ, 3σ ellipse radii).
pub const COVERAGE_THRESHOLDS: [f64; 3] = [1.0, 4.0, 9.0];

/// Trapezoidal nodes of the log-\\( w \\) \\( w \\)-rule (built once per
/// \\( \nu \\)). 512 nodes over the quantile bracket hold both the
/// \\( w \\)-integral of the reference CDF and the predictive density below
/// \\( 10^{-6} \\) across the tested parameter box (verified against an
/// independent fine Simpson reference).
pub const W_NODES: usize = 512;

/// The two-sided tail probability the log-\\( w \\) quadrature bracket
/// excludes: the Gamma mass outside \\( [Q(p), Q(1-p)] \\), negligible against
/// the \\( 10^{-6} \\) target.
pub const W_BRACKET_TAIL: f64 = 1e-13;

/// Minimum midpoint nodes of the adaptive \\( \theta \\) rule for the
/// reference CDF (used in the near-isotropic case).
pub const THETA_MIN_NODES: usize = 128;

/// Maximum midpoint nodes of the adaptive \\( \theta \\) rule (an anisotropy
/// so extreme it needs more is capped here).
pub const THETA_MAX_NODES: usize = 4096;

/// A scoring failure, split by the axis that failed so a caller can name it.
#[derive(Debug, Clone, PartialEq)]
pub enum LawError {
    /// \\( C = \Sigma_{obs} + \Sigma_{pred} \\) is not positive definite.
    NonPdCombined,
    /// The residual (or a covariance entry) is not finite.
    NonFiniteResidual,
    /// A Student-t law was requested with \\( \nu \le 2 \\) or a non-finite
    /// \\( \nu \\); no finite covariance exists there.
    NuOutOfRange(f64),
}

impl std::fmt::Display for LawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LawError::NonPdCombined => {
                write!(
                    f,
                    "combined covariance Σ_obs+Σ_pred is not positive definite"
                )
            }
            LawError::NonFiniteResidual => {
                write!(f, "residual or covariance entry is not finite")
            }
            LawError::NuOutOfRange(nu) => {
                write!(f, "student-t ν={nu} out of range (need ν > 2 and finite)")
            }
        }
    }
}

/// Which predictive law scores a row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LawKind {
    /// Gaussian residual, \\( \chi^2_2 \\) reference.
    Normal,
    /// Joint bivariate Student-t, \\( \nu \\) degrees of freedom.
    StudentT {
        /// Degrees of freedom \\( \nu > 2 \\).
        nu: f64,
    },
}

/// A prepared law with its \\( w \\)-quadrature nodes (built once per
/// \\( \nu \\); reused across every row of a series).
#[derive(Debug, Clone)]
pub struct LawQuad {
    kind: LawKind,
    /// Quadrature weights \\( \omega_i \\), summing to 1 (empty for normal).
    wt: Vec<f64>,
    /// \\( s(w_i) = (\nu-2)/(\nu w_i) \\), the per-node observation scale.
    s: Vec<f64>,
}

/// The per-row scoring products under one law.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowScore {
    /// Log predictive density \\( \ln p(r) \\).
    pub log_score: f64,
    /// PIT value \\( u = F(d^2) \\) under the reference law.
    pub pit: f64,
    /// Reference CDF \\( F(c) \\) at each of [`COVERAGE_THRESHOLDS`] — the
    /// per-row law-aware expected coverage.
    pub expected_coverage: [f64; 3],
}

impl LawQuad {
    /// The normal law (no quadrature needed).
    pub fn normal() -> Self {
        LawQuad {
            kind: LawKind::Normal,
            wt: Vec::new(),
            s: Vec::new(),
        }
    }

    /// A Student-t law with \\( \nu \\) degrees of freedom, building the
    /// generalized Gauss–Laguerre \\( w \\)-nodes once.
    ///
    /// Errors with [`LawError::NuOutOfRange`] for \\( \nu \le 2 \\) or a
    /// non-finite \\( \nu \\).
    pub fn student_t(nu: f64) -> Result<Self, LawError> {
        if !nu.is_finite() || nu <= 2.0 {
            return Err(LawError::NuOutOfRange(nu));
        }
        let a = nu / 2.0; // Gamma shape
        let b = nu / 2.0; // Gamma rate
        // Trapezoid in ξ = ln w over the Gamma-quantile bracket; ω_k =
        // g(w_k)·w_k·(trapezoid coefficient)·Δξ, then normalized so Σ ω_k = 1
        // (the exact Gamma mass on [0,∞) is 1; this removes bracket/truncation
        // bias). ∫F(w)g(w)dw ≈ Σ ω_k F(w_k).
        let xi_lo = (gamma_quantile_reg(a, W_BRACKET_TAIL) / b).ln();
        let xi_hi = (gamma_quantile_reg(a, 1.0 - W_BRACKET_TAIL) / b).ln();
        let n = W_NODES;
        let dxi = (xi_hi - xi_lo) / n as f64;
        let ln_norm = a * b.ln() - ln_gamma(a); // ln of the Gamma prefactor b^a/Γ(a)
        let mut wt = Vec::with_capacity(n + 1);
        let mut s = Vec::with_capacity(n + 1);
        for k in 0..=n {
            let xi = xi_lo + k as f64 * dxi;
            let w = xi.exp();
            // g(w)·w = exp(ln_norm + (a-1)ln w - b w)·w = exp(ln_norm + a·ξ − b·w)
            let integrand = (ln_norm + a * xi - b * w).exp();
            let trap = if k == 0 || k == n { 0.5 } else { 1.0 };
            wt.push(integrand * trap * dxi);
            s.push((nu - 2.0) / (nu * w));
        }
        let tot: f64 = wt.iter().sum();
        for x in wt.iter_mut() {
            *x /= tot;
        }
        Ok(LawQuad {
            kind: LawKind::StudentT { nu },
            wt,
            s,
        })
    }

    /// Build the law for [`LawKind`], validating \\( \nu \\).
    pub fn for_kind(kind: LawKind) -> Result<Self, LawError> {
        match kind {
            LawKind::Normal => Ok(Self::normal()),
            LawKind::StudentT { nu } => Self::student_t(nu),
        }
    }

    /// The law this quadrature scores under.
    pub fn kind(&self) -> LawKind {
        self.kind
    }

    /// The log predictive density \\( \ln p(r) \\) under this law.
    pub fn log_density(
        &self,
        r: [f64; 2],
        sigma_obs: &Mat2,
        sigma_pred: &Mat2,
    ) -> Result<f64, LawError> {
        check_inputs(r, sigma_obs, sigma_pred)?;
        match self.kind {
            LawKind::Normal => {
                let c = add2(sigma_obs, sigma_pred);
                let (cinv, _) = inv2_det(&c).ok_or(LawError::NonPdCombined)?;
                let d2 = quad(&cinv, r);
                Ok(-0.5 * (d2 + ln_det2(&c) + 2.0 * (2.0 * std::f64::consts::PI).ln()))
            }
            LawKind::StudentT { .. } => Ok(log_sum_exp(
                &self.student_ln_terms(r, sigma_obs, sigma_pred)?,
            )),
        }
    }

    /// The reference CDF \\( F(c) \\) of \\( d^2 \\) under this law, given the
    /// row's covariances.
    pub fn reference_cdf(
        &self,
        c: f64,
        sigma_obs: &Mat2,
        sigma_pred: &Mat2,
    ) -> Result<f64, LawError> {
        match self.kind {
            LawKind::Normal => Ok(chi2_2_cdf(c)),
            LawKind::StudentT { .. } => {
                let cc = add2(sigma_obs, sigma_pred);
                let (cinv, _) = inv2_det(&cc).ok_or(LawError::NonPdCombined)?;
                let lam = self.student_lambdas(&cinv, sigma_obs, sigma_pred)?;
                Ok(self.mixture_cdf(c, &lam))
            }
        }
    }

    /// The PIT \\( u = F(d^2) \\) of a residual under this law.
    pub fn pit(&self, r: [f64; 2], sigma_obs: &Mat2, sigma_pred: &Mat2) -> Result<f64, LawError> {
        check_inputs(r, sigma_obs, sigma_pred)?;
        let cc = add2(sigma_obs, sigma_pred);
        let (cinv, _) = inv2_det(&cc).ok_or(LawError::NonPdCombined)?;
        let d2 = quad(&cinv, r);
        self.reference_cdf(d2, sigma_obs, sigma_pred)
    }

    /// Score a residual `r` against \\( \Sigma_{obs} \\) and
    /// \\( \Sigma_{pred} \\): the log predictive density, the PIT of the
    /// realized \\( d^2 \\), and \\( F(c) \\) at [`COVERAGE_THRESHOLDS`]. The
    /// eigen-decomposition per \\( w \\)-node is shared across the four CDF
    /// evaluations.
    pub fn score_row(
        &self,
        r: [f64; 2],
        sigma_obs: &Mat2,
        sigma_pred: &Mat2,
    ) -> Result<RowScore, LawError> {
        check_inputs(r, sigma_obs, sigma_pred)?;
        let c = add2(sigma_obs, sigma_pred);
        let (cinv, _) = inv2_det(&c).ok_or(LawError::NonPdCombined)?;
        let d2 = quad(&cinv, r);
        match self.kind {
            LawKind::Normal => {
                let log_score = -0.5 * (d2 + ln_det2(&c) + 2.0 * (2.0 * std::f64::consts::PI).ln());
                Ok(RowScore {
                    log_score,
                    pit: chi2_2_cdf(d2),
                    expected_coverage: [
                        chi2_2_cdf(COVERAGE_THRESHOLDS[0]),
                        chi2_2_cdf(COVERAGE_THRESHOLDS[1]),
                        chi2_2_cdf(COVERAGE_THRESHOLDS[2]),
                    ],
                })
            }
            LawKind::StudentT { .. } => {
                let ln2pi = (2.0 * std::f64::consts::PI).ln();
                let mut ln_terms: Vec<f64> = Vec::with_capacity(self.s.len());
                let mut lam: Vec<(f64, f64)> = Vec::with_capacity(self.s.len());
                for (&si, &wi) in self.s.iter().zip(self.wt.iter()) {
                    let a_w = add2(sigma_pred, &scale2(sigma_obs, si));
                    let (a_inv, _) = inv2_det(&a_w).ok_or(LawError::NonPdCombined)?;
                    ln_terms.push(wi.ln() - ln2pi - 0.5 * ln_det2(&a_w) - 0.5 * quad(&a_inv, r));
                    lam.push(gen_eig2(&cinv, &a_w));
                }
                Ok(RowScore {
                    log_score: log_sum_exp(&ln_terms),
                    pit: self.mixture_cdf(d2, &lam),
                    expected_coverage: [
                        self.mixture_cdf(COVERAGE_THRESHOLDS[0], &lam),
                        self.mixture_cdf(COVERAGE_THRESHOLDS[1], &lam),
                        self.mixture_cdf(COVERAGE_THRESHOLDS[2], &lam),
                    ],
                })
            }
        }
    }

    /// Per-node \\( \ln[\omega_i\,\mathcal N(r;0,A_i)] \\) for the Student-t
    /// density. `A_i = Σ_pred + s_i Σ_obs`.
    fn student_ln_terms(
        &self,
        r: [f64; 2],
        sigma_obs: &Mat2,
        sigma_pred: &Mat2,
    ) -> Result<Vec<f64>, LawError> {
        let ln2pi = (2.0 * std::f64::consts::PI).ln();
        let mut out = Vec::with_capacity(self.s.len());
        for (&si, &wi) in self.s.iter().zip(self.wt.iter()) {
            let a_w = add2(sigma_pred, &scale2(sigma_obs, si));
            let (a_inv, _) = inv2_det(&a_w).ok_or(LawError::NonPdCombined)?;
            out.push(wi.ln() - ln2pi - 0.5 * ln_det2(&a_w) - 0.5 * quad(&a_inv, r));
        }
        Ok(out)
    }

    /// Per-node generalized eigenvalues \\( (\lambda_1, \lambda_2) \\) of
    /// \\( C^{-1} A_i \\), for the reference CDF.
    fn student_lambdas(
        &self,
        cinv: &Mat2,
        sigma_obs: &Mat2,
        sigma_pred: &Mat2,
    ) -> Result<Vec<(f64, f64)>, LawError> {
        let mut out = Vec::with_capacity(self.s.len());
        for &si in &self.s {
            let a_w = add2(sigma_pred, &scale2(sigma_obs, si));
            out.push(gen_eig2(cinv, &a_w));
        }
        Ok(out)
    }

    /// \\( F(c) = \sum_i \omega_i P(d^2\le c\mid w_i) \\) from the per-node
    /// eigenvalues.
    fn mixture_cdf(&self, c: f64, lam: &[(f64, f64)]) -> f64 {
        let mut acc = 0.0;
        for (&wi, &(l1, l2)) in self.wt.iter().zip(lam.iter()) {
            acc += wi * self.cond_cdf(c, l1, l2);
        }
        acc.clamp(0.0, 1.0)
    }

    /// \\( P(d^2 \le c \mid w) \\) for eigenvalues \\( \lambda_1,\lambda_2 \\)
    /// via the \\( \theta \\) midpoint rule (period \\( \pi \\)), with the
    /// isotropic (\\( \lambda_1=\lambda_2 \\)) closed form short-circuited and
    /// the node count adapted to the eigenvalue ratio — a sharply anisotropic
    /// integrand (a narrow \\( q(\theta) \\) trough) needs more nodes.
    fn cond_cdf(&self, c: f64, l1: f64, l2: f64) -> f64 {
        if c <= 0.0 {
            return 0.0;
        }
        let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        if lo <= 0.0 {
            return 0.0;
        }
        if (hi - lo) <= 1e-12 * hi {
            return 1.0 - (-c / (2.0 * hi)).exp();
        }
        // The midpoint rule converges geometrically, but the analytic strip
        // narrows as the q(θ) trough sharpens (width ~ sqrt(lo/hi)); a node
        // count growing with sqrt(ratio) keeps the error below 1e-6.
        let ratio = hi / lo;
        let m = ((120.0 * ratio.sqrt()) as usize)
            .clamp(THETA_MIN_NODES, THETA_MAX_NODES)
            .next_multiple_of(2);
        let mut acc = 0.0;
        for j in 0..m {
            let th = (j as f64 + 0.5) * std::f64::consts::PI / m as f64;
            let (sin, cos) = th.sin_cos();
            let q = hi * cos * cos + lo * sin * sin;
            acc += 1.0 - (-c / (2.0 * q)).exp();
        }
        acc / m as f64
    }
}

/// The reference-distribution name a scoring law records on its rows.
/// The normal law keeps `"chi2_2"` (its \\( d^2 \\) reference genuinely is
/// \\( \chi^2_2 \\)); a Student-t law names its elliptical mixture with
/// \\( \nu \\).
pub fn reference_law_name(kind: LawKind) -> String {
    match kind {
        LawKind::Normal => "chi2_2".to_string(),
        LawKind::StudentT { nu } => format!("student_t(nu={nu})"),
    }
}

/// The scoring-law tag recorded on a row (`"normal"` | `"student-t"`).
pub fn scoring_law_tag(kind: LawKind) -> String {
    match kind {
        LawKind::Normal => "normal".to_string(),
        LawKind::StudentT { .. } => "student-t".to_string(),
    }
}

// ---------------------------------------------------------------------------
// 2×2 linear algebra
// ---------------------------------------------------------------------------

/// Validate a residual and its two covariances are finite.
fn check_inputs(r: [f64; 2], so: &Mat2, sp: &Mat2) -> Result<(), LawError> {
    if r[0].is_finite() && r[1].is_finite() && finite_mat(so) && finite_mat(sp) {
        Ok(())
    } else {
        Err(LawError::NonFiniteResidual)
    }
}

fn finite_mat(m: &Mat2) -> bool {
    m.iter().all(|row| row.iter().all(|x| x.is_finite()))
}

fn add2(a: &Mat2, b: &Mat2) -> Mat2 {
    [
        [a[0][0] + b[0][0], a[0][1] + b[0][1]],
        [a[1][0] + b[1][0], a[1][1] + b[1][1]],
    ]
}

fn scale2(a: &Mat2, s: f64) -> Mat2 {
    [[a[0][0] * s, a[0][1] * s], [a[1][0] * s, a[1][1] * s]]
}

/// Inverse and determinant of a symmetric positive-definite 2×2, or `None`
/// when it is not positive definite (a leading-minor / determinant check).
fn inv2_det(m: &Mat2) -> Option<(Mat2, f64)> {
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    if !(m[0][0] > 0.0 && det > 0.0 && det.is_finite()) {
        return None;
    }
    let inv = [
        [m[1][1] / det, -m[0][1] / det],
        [-m[1][0] / det, m[0][0] / det],
    ];
    Some((inv, det))
}

/// \\( \ln\det \\) of a symmetric 2×2.
fn ln_det2(m: &Mat2) -> f64 {
    (m[0][0] * m[1][1] - m[0][1] * m[1][0]).ln()
}

/// Quadratic form \\( r^\top M r \\) with `m` a 2×2 matrix.
fn quad(m: &Mat2, r: [f64; 2]) -> f64 {
    r[0] * (m[0][0] * r[0] + m[0][1] * r[1]) + r[1] * (m[1][0] * r[0] + m[1][1] * r[1])
}

/// Generalized eigenvalues \\( \lambda \\) solving
/// \\( \det(A - \lambda C) = 0 \\), i.e. the eigenvalues of
/// \\( C^{-1}A \\) — real because \\( C^{-1}A \\) is similar to the symmetric
/// \\( C^{-1/2} A C^{-1/2} \\). `cinv` is \\( C^{-1} \\).
fn gen_eig2(cinv: &Mat2, a: &Mat2) -> (f64, f64) {
    // M = C⁻¹ A (generally non-symmetric, real eigenvalues).
    let m = [
        [
            cinv[0][0] * a[0][0] + cinv[0][1] * a[1][0],
            cinv[0][0] * a[0][1] + cinv[0][1] * a[1][1],
        ],
        [
            cinv[1][0] * a[0][0] + cinv[1][1] * a[1][0],
            cinv[1][0] * a[0][1] + cinv[1][1] * a[1][1],
        ],
    ];
    let tr = m[0][0] + m[1][1];
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    let disc = (tr * tr - 4.0 * det).max(0.0).sqrt();
    (0.5 * (tr + disc), 0.5 * (tr - disc))
}

/// Numerically stable \\( \ln\sum_i e^{x_i} \\).
fn log_sum_exp(xs: &[f64]) -> f64 {
    let m = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if !m.is_finite() {
        return m;
    }
    let s: f64 = xs.iter().map(|x| (x - m).exp()).sum();
    m + s.ln()
}

/// \\( \chi^2_2 \\) CDF, \\( F(c) = 1 - e^{-c/2} \\).
fn chi2_2_cdf(c: f64) -> f64 {
    if c <= 0.0 {
        0.0
    } else {
        1.0 - (-c / 2.0).exp()
    }
}

// ---------------------------------------------------------------------------
// Gamma quantile — the log-w quadrature bracket (and the tests' reference)
// ---------------------------------------------------------------------------

/// Quantile of the regularized lower incomplete gamma: return `x` such that
/// \\( P(a, x) = u \\), where \\( P = 1 - Q \\) and \\( Q \\) is the
/// regularized upper incomplete gamma. Newton with a bisection safety net; the
/// density \\( P'(a,x) = x^{a-1} e^{-x}/\Gamma(a) \\) is known in closed form.
/// Used to bracket the log-\\( w \\) quadrature (once per \\( \nu \\)).
fn gamma_quantile_reg(a: f64, u: f64) -> f64 {
    use hyperjet::statistics::upper_inc_gamma_reg;
    let u = u.clamp(1e-15, 1.0 - 1e-15);
    // Bracket: grow an upper bound until P(a, hi) > u.
    let mut lo = 0.0_f64;
    let mut hi = (a + 5.0 * a.sqrt()).max(1.0);
    while 1.0 - upper_inc_gamma_reg(a, hi) < u {
        hi *= 2.0;
        if hi > 1e300 {
            break;
        }
    }
    // Wilson–Hilferty start, clamped into the bracket.
    let mut x = {
        let g = 1.0 - 2.0 / (9.0 * a);
        let z = inv_norm(u);
        let wh = a * (g + z * (2.0 / (9.0 * a)).sqrt()).powi(3);
        if wh.is_finite() && wh > 0.0 {
            wh.clamp(lo + 1e-12, hi)
        } else {
            0.5 * (lo + hi)
        }
    };
    let ln_g = ln_gamma(a);
    for _ in 0..100 {
        let f = (1.0 - upper_inc_gamma_reg(a, x)) - u; // P(a,x) - u
        if f > 0.0 {
            hi = x;
        } else {
            lo = x;
        }
        let pdf = if x > 0.0 {
            ((a - 1.0) * x.ln() - x - ln_g).exp()
        } else {
            0.0
        };
        let step = if pdf > 0.0 { f / pdf } else { 0.0 };
        let mut nx = x - step;
        if !(nx > lo && nx < hi) {
            nx = 0.5 * (lo + hi); // bisection fallback
        }
        if (nx - x).abs() <= 1e-13 * (1.0 + x.abs()) {
            return nx;
        }
        x = nx;
    }
    x
}

/// Inverse standard-normal CDF (Acklam's rational approximation), used only
/// to seed the Gamma-quantile Newton iteration.
fn inv_norm(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let pl = 0.02425;
    if p < pl {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - pl {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

// ---------------------------------------------------------------------------
// Cramér–von Mises calibration statistic
// ---------------------------------------------------------------------------

/// Cramér–von Mises statistic \\( W^2 \\) of PIT values against
/// \\( \mathrm{Uniform}(0,1) \\):
/// \\[ W^2 = \frac1{12n} + \sum_{i=1}^n\left(u_{(i)} -
///     \frac{2i-1}{2n}\right)^2, \\]
/// with \\( u_{(i)} \\) the ascending order statistics. The asymptotic 5%
/// critical value for a fully specified null is \\( \approx 0.461 \\)
/// (a value the tests verify against, never a threshold applied in the
/// kernel). Returns `None` for an empty sample.
pub fn cramer_von_mises_uniform(pits: &[f64]) -> Option<f64> {
    if pits.is_empty() {
        return None;
    }
    let mut u: Vec<f64> = pits.iter().map(|x| x.clamp(0.0, 1.0)).collect();
    u.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = u.len() as f64;
    let mut acc = 1.0 / (12.0 * n);
    for (i, &ui) in u.iter().enumerate() {
        let expected = (2.0 * (i as f64 + 1.0) - 1.0) / (2.0 * n);
        acc += (ui - expected).powi(2);
    }
    Some(acc)
}

/// The 20-bin PIT histogram on \\( [0,1] \\) (a value of exactly 1 falls in
/// the last bin). Non-finite values are dropped.
pub fn pit_histogram(pits: &[f64], bins: usize) -> Vec<u32> {
    let mut h = vec![0u32; bins];
    for &p in pits {
        if !p.is_finite() {
            continue;
        }
        let b = (p.clamp(0.0, 1.0) * bins as f64) as usize;
        h[b.min(bins - 1)] += 1;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- deterministic PRNG + generative sampler for the MC checks --------

    /// SplitMix64 uniform stream; seeded, deterministic.
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Self {
            Rng(seed)
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn unif(&mut self) -> f64 {
            ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }
        fn normal(&mut self) -> f64 {
            let u1 = self.unif();
            let u2 = self.unif();
            (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        }
        /// Gamma(shape a ≥ 1, rate 1) via Marsaglia–Tsang.
        fn gamma(&mut self, a: f64) -> f64 {
            assert!(a >= 1.0);
            let d = a - 1.0 / 3.0;
            let c = 1.0 / (9.0 * d).sqrt();
            loop {
                let x = self.normal();
                let v = (1.0 + c * x).powi(3);
                if v <= 0.0 {
                    continue;
                }
                let u = self.unif();
                if u.ln() < 0.5 * x * x + d - d * v + d * v.ln() {
                    return d * v;
                }
            }
        }
    }

    fn chol(m: &Mat2) -> Mat2 {
        let l00 = m[0][0].sqrt();
        let l10 = m[1][0] / l00;
        let l11 = (m[1][1] - l10 * l10).sqrt();
        [[l00, 0.0], [l10, l11]]
    }

    /// One draw of the generative model: r = e_obs + e_pred with the joint
    /// Student-t observation mixture (normal when `nu` is None).
    fn draw(rng: &mut Rng, so: &Mat2, sp: &Mat2, nu: Option<f64>) -> [f64; 2] {
        let lo = chol(so);
        let lp = chol(sp);
        let z0 = rng.normal();
        let z1 = rng.normal();
        let s = match nu {
            Some(nu) => {
                let w = rng.gamma(nu / 2.0) / (nu / 2.0); // Gamma(ν/2, rate ν/2)
                (nu - 2.0) / (nu * w)
            }
            None => 1.0,
        };
        let ss = s.sqrt();
        let e_obs = [ss * lo[0][0] * z0, ss * (lo[1][0] * z0 + lo[1][1] * z1)];
        let z2 = rng.normal();
        let z3 = rng.normal();
        let e_pred = [lp[0][0] * z2, lp[1][0] * z2 + lp[1][1] * z3];
        [e_obs[0] + e_pred[0], e_obs[1] + e_pred[1]]
    }

    // -- independent fine reference integrators (composite Simpson) -------

    /// Composite Simpson integration in w of `f(w)·g(w)` over an extreme
    /// quantile bracket — an INDEPENDENT method (Simpson, not Gauss–Legendre;
    /// no PIT transform), for the accuracy tests only. Integrated in log-w so
    /// the heavy small-ν tail is resolved.
    fn ref_integral(nu: f64, n: usize, f: &dyn Fn(f64) -> f64) -> f64 {
        let a = nu / 2.0;
        let b = nu / 2.0;
        let w_lo = gamma_quantile_reg(a, 1e-12) / b;
        let w_hi = gamma_quantile_reg(a, 1.0 - 1e-12) / b;
        let ln_g = ln_gamma(a);
        let g = |w: f64| -> f64 {
            if w <= 0.0 {
                0.0
            } else {
                (a * b.ln() + (a - 1.0) * w.ln() - b * w - ln_g).exp()
            }
        };
        let llo = w_lo.ln();
        let lhi = w_hi.ln();
        let dl = (lhi - llo) / n as f64;
        // integrand in log-w: f(w) g(w) w
        let val = |i: usize| -> f64 {
            let w = (llo + i as f64 * dl).exp();
            f(w) * g(w) * w
        };
        let mut acc = val(0) + val(n);
        for i in 1..n {
            acc += if i % 2 == 1 { 4.0 } else { 2.0 } * val(i);
        }
        acc * dl / 3.0
    }

    fn ref_density(nu: f64, r: [f64; 2], so: &Mat2, sp: &Mat2) -> f64 {
        let f = |w: f64| -> f64 {
            let s = (nu - 2.0) / (nu * w);
            let a_w = add2(sp, &scale2(so, s));
            let (inv, _) = inv2_det(&a_w).unwrap();
            (-(2.0 * std::f64::consts::PI).ln() - 0.5 * ln_det2(&a_w) - 0.5 * quad(&inv, r)).exp()
        };
        ref_integral(nu, 20_000, &f)
    }

    fn ref_cdf(nu: f64, c: f64, so: &Mat2, sp: &Mat2) -> f64 {
        let cc = add2(so, sp);
        let (cinv, _) = inv2_det(&cc).unwrap();
        let f = |w: f64| -> f64 {
            let s = (nu - 2.0) / (nu * w);
            let a_w = add2(sp, &scale2(so, s));
            let (l1, l2) = gen_eig2(&cinv, &a_w);
            let m = 1024usize;
            let mut acc = 0.0;
            for j in 0..m {
                let th = (j as f64 + 0.5) * std::f64::consts::PI / m as f64;
                let q = l1 * th.cos().powi(2) + l2 * th.sin().powi(2);
                acc += 1.0 - (-c / (2.0 * q)).exp();
            }
            acc / m as f64
        };
        ref_integral(nu, 8000, &f)
    }

    // -- (a) the four limiting cases -------------------------------------

    #[test]
    fn limit_sigma_pred_to_zero_matches_closed_form() {
        let nu = 4.0;
        let law = LawQuad::student_t(nu).unwrap();
        let so = [[0.4, 0.05], [0.05, 0.6]];
        let sp = [[1e-12, 0.0], [0.0, 1e-12]];
        for &c in &[0.5, 1.0, 4.0, 9.0] {
            let closed = 1.0 - (1.0 + c / (nu - 2.0)).powf(-nu / 2.0);
            let f = law.reference_cdf(c, &so, &sp).unwrap();
            assert!((f - closed).abs() < 1e-5, "c={c}: {f} vs closed {closed}");
        }
        // Joint elliptical-t density closed form at a point.
        let r = [0.3, -0.2];
        let m2 = quad(&inv2_det(&so).unwrap().0, r);
        let ln_closed = ln_gamma((nu + 2.0) / 2.0)
            - ln_gamma(nu / 2.0)
            - (std::f64::consts::PI * (nu - 2.0)).ln()
            - 0.5 * ln_det2(&so)
            - (nu + 2.0) / 2.0 * (1.0 + m2 / (nu - 2.0)).ln();
        let ld = law.log_density(r, &so, &sp).unwrap();
        assert!(
            (ld - ln_closed).abs() < 1e-5,
            "log density {ld} vs {ln_closed}"
        );
    }

    #[test]
    fn limit_sigma_obs_to_zero_is_chi2_2_and_gaussian() {
        let law = LawQuad::student_t(4.0).unwrap();
        let so = [[1e-12, 0.0], [0.0, 1e-12]];
        let sp = [[0.5, 0.1], [0.1, 0.7]];
        let r = [0.2, 0.3];
        let sc = law.score_row(r, &so, &sp).unwrap();
        let cc = add2(&so, &sp);
        let d2 = quad(&inv2_det(&cc).unwrap().0, r);
        assert!((sc.pit - chi2_2_cdf(d2)).abs() < 1e-6);
        let gauss = -0.5 * (d2 + ln_det2(&cc) + 2.0 * (2.0 * std::f64::consts::PI).ln());
        assert!((sc.log_score - gauss).abs() < 1e-5);
        for (k, &c) in COVERAGE_THRESHOLDS.iter().enumerate() {
            assert!((sc.expected_coverage[k] - chi2_2_cdf(c)).abs() < 1e-6);
        }
    }

    #[test]
    fn limit_large_nu_approaches_normal() {
        // The t↔normal gap is genuinely O(1/ν) (not a quadrature error), so a
        // large ν makes it vanish: ν = 2000 closes it to well under 2e-3.
        let law = LawQuad::student_t(2000.0).unwrap();
        let normal = LawQuad::normal();
        let so = [[0.4, 0.05], [0.05, 0.6]];
        let sp = [[0.3, -0.05], [-0.05, 0.2]];
        let r = [0.35, -0.25];
        let t = law.score_row(r, &so, &sp).unwrap();
        let n = normal.score_row(r, &so, &sp).unwrap();
        assert!((t.pit - n.pit).abs() < 1e-3, "pit {} vs {}", t.pit, n.pit);
        assert!(
            (t.log_score - n.log_score).abs() < 2e-3,
            "log {} vs {}",
            t.log_score,
            n.log_score
        );
    }

    #[test]
    fn log_w_quadrature_reproduces_the_gamma_moments() {
        // The log-w rule must integrate the Gamma(ν/2, ν/2) weight to unit
        // mass and recover E[w] = 1, E[w²] = 1 + 2/ν (w_i = (ν−2)/(ν s_i)).
        for &nu in &[2.5_f64, 4.0, 100.0] {
            let law = LawQuad::student_t(nu).unwrap();
            assert!(
                (law.wt.iter().sum::<f64>() - 1.0).abs() < 1e-12,
                "Σω ν={nu}"
            );
            let w = |s: f64| (nu - 2.0) / (nu * s);
            let m1: f64 = law
                .wt
                .iter()
                .zip(law.s.iter())
                .map(|(o, s)| o * w(*s))
                .sum();
            let m2: f64 = law
                .wt
                .iter()
                .zip(law.s.iter())
                .map(|(o, s)| o * w(*s).powi(2))
                .sum();
            assert!((m1 - 1.0).abs() < 1e-8, "E[w] ν={nu}: {m1}");
            assert!((m2 - (1.0 + 2.0 / nu)).abs() < 1e-7, "E[w²] ν={nu}: {m2}");
        }
    }

    #[test]
    fn limit_proportional_covariances_collapse_theta() {
        let nu = 5.0;
        let law = LawQuad::student_t(nu).unwrap();
        let so = [[0.5, 0.1], [0.1, 0.4]];
        let sp = scale2(&so, 3.0);
        for &c in &COVERAGE_THRESHOLDS {
            let got = law.reference_cdf(c, &so, &sp).unwrap();
            let rf = ref_cdf(nu, c, &so, &sp);
            assert!((got - rf).abs() < 1e-6, "c={c}: {got} vs ref {rf}");
        }
    }

    // -- (c) quadrature accuracy against the fine reference --------------

    #[test]
    fn quadrature_accuracy_over_the_parameter_box() {
        let mut worst_cdf = 0.0_f64;
        let mut worst_p = 0.0_f64;
        let mut where_cdf = (0.0, 0.0, 0.0);
        let mut where_p = (0.0, 0.0, 0.0);
        for &nu in &[2.5_f64, 3.0, 8.0, 100.0] {
            let law = LawQuad::student_t(nu).unwrap();
            for &tr in &[1e-4_f64, 1.0, 1e4] {
                // Σ_obs anisotropic (axis ratio 100); Σ_pred rotated, scaled
                // to `tr` times Σ_obs's trace.
                let so = [[1.0, 0.0], [0.0, 0.01]];
                let tr_obs = so[0][0] + so[1][1];
                let k = tr * tr_obs / 2.0;
                let sp = [[k * 1.3, k * 0.2], [k * 0.2, k * 0.7]];
                let cc = add2(&so, &sp);
                let (cinv, _) = inv2_det(&cc).unwrap();
                // Density is tested across the SCORED regime: residuals on the
                // d² = 1, 4, 9 (1σ/2σ/3σ) contours of the combined covariance,
                // in two directions. (For a wildly out-of-range residual — a
                // flagged 10σ outlier, never scored — the density's importance
                // peak sits below the Gamma-quantile bracket and neither this
                // rule nor a fixed-bracket reference resolves it; that regime
                // carries no scoring weight.)
                for &d2t in &COVERAGE_THRESHOLDS {
                    for base in [[1.0_f64, 0.3], [0.4, -1.0]] {
                        let d0 = quad(&cinv, base);
                        let sc = (d2t / d0).sqrt();
                        let r = [base[0] * sc, base[1] * sc];
                        let p_got = law.log_density(r, &so, &sp).unwrap().exp();
                        let p_ref = ref_density(nu, r, &so, &sp);
                        let e = (p_got - p_ref).abs() / p_ref.max(1e-300);
                        if e > worst_p {
                            worst_p = e;
                            where_p = (nu, tr, d2t);
                        }
                    }
                }
                for &c in &COVERAGE_THRESHOLDS {
                    let got = law.reference_cdf(c, &so, &sp).unwrap();
                    let rf = ref_cdf(nu, c, &so, &sp);
                    let e = (got - rf).abs();
                    if e > worst_cdf {
                        worst_cdf = e;
                        where_cdf = (nu, tr, c);
                    }
                }
            }
        }
        eprintln!(
            "quadrature accuracy: worst |ΔF| = {worst_cdf:.2e} at (ν,tr,c)={where_cdf:?}; \
             worst density rel err = {worst_p:.2e} at (ν,tr,d²)={where_p:?}"
        );
        assert!(
            worst_cdf < 1e-6,
            "worst CDF abs error {worst_cdf} at {where_cdf:?}"
        );
        assert!(
            worst_p < 1e-6,
            "worst density rel error {worst_p} at {where_p:?}"
        );
    }

    // -- (b) density and CDF vs seeded Monte Carlo -----------------------

    #[test]
    fn cdf_matches_monte_carlo_generative_model() {
        let nu = 4.0;
        let law = LawQuad::student_t(nu).unwrap();
        let so = [[0.6, 0.1], [0.1, 0.3]];
        let sp = [[0.2, -0.03], [-0.03, 0.5]];
        let n = 400_000usize;
        let mut rng = Rng::new(0x00C0_FFEE);
        let cinv = inv2_det(&add2(&so, &sp)).unwrap().0;
        let mut counts = [0u32; 3];
        for _ in 0..n {
            let r = draw(&mut rng, &so, &sp, Some(nu));
            let d2 = quad(&cinv, r);
            for (k, &c) in COVERAGE_THRESHOLDS.iter().enumerate() {
                if d2 <= c {
                    counts[k] += 1;
                }
            }
        }
        for (k, &c) in COVERAGE_THRESHOLDS.iter().enumerate() {
            let f = law.reference_cdf(c, &so, &sp).unwrap();
            let emp = counts[k] as f64 / n as f64;
            let se = (f * (1.0 - f) / n as f64).sqrt();
            assert!((emp - f).abs() < 4.0 * se + 1e-4, "c={c}: emp {emp} vs {f}");
        }
    }

    #[test]
    fn density_normalizes_to_one() {
        for law in [LawQuad::normal(), LawQuad::student_t(3.0).unwrap()] {
            let so = [[0.5, 0.05], [0.05, 0.4]];
            let sp = [[0.3, 0.0], [0.0, 0.6]];
            let lim = 30.0;
            let m = 500usize;
            let h = 2.0 * lim / m as f64;
            let mut acc = 0.0;
            for i in 0..m {
                let x = -lim + (i as f64 + 0.5) * h;
                for j in 0..m {
                    let y = -lim + (j as f64 + 0.5) * h;
                    acc += law.log_density([x, y], &so, &sp).unwrap().exp() * h * h;
                }
            }
            assert!((acc - 1.0).abs() < 1e-3, "∫p = {acc}");
        }
    }

    // -- (d) PIT uniformity on matched data, non-uniform on mismatched ---

    #[test]
    fn pits_are_uniform_on_matched_data() {
        let mut rng = Rng::new(0x1234_5678);
        for &tr in &[0.1_f64, 1.0, 10.0] {
            let so = [[0.5, 0.05], [0.05, 0.4]];
            let sp = scale2(&[[0.6, 0.1], [0.1, 0.9]], tr);
            let law = LawQuad::normal();
            let mut pits = Vec::new();
            for _ in 0..3000 {
                let r = draw(&mut rng, &so, &sp, None);
                pits.push(law.pit(r, &so, &sp).unwrap());
            }
            assert!(
                cramer_von_mises_uniform(&pits).unwrap() < 0.461,
                "normal tr={tr}"
            );
        }
        for &tr in &[0.1_f64, 1.0, 10.0] {
            let so = [[0.5, 0.05], [0.05, 0.4]];
            let sp = scale2(&[[0.6, 0.1], [0.1, 0.9]], tr);
            let law = LawQuad::student_t(4.0).unwrap();
            let mut pits = Vec::new();
            for _ in 0..3000 {
                let r = draw(&mut rng, &so, &sp, Some(4.0));
                pits.push(law.pit(r, &so, &sp).unwrap());
            }
            assert!(
                cramer_von_mises_uniform(&pits).unwrap() < 0.461,
                "t4 tr={tr}"
            );
        }
    }

    #[test]
    fn pits_are_not_uniform_on_mismatched_data() {
        // Normal scoring of joint-t4 data at a small trace ratio (obs-dominated,
        // where the law matters most): heavy tails ⇒ PIT piles at the edges.
        let mut rng = Rng::new(0x0000_9999);
        let so = [[0.5, 0.05], [0.05, 0.4]];
        let sp = scale2(&so, 0.05);
        let normal = LawQuad::normal();
        let mut pits = Vec::new();
        for _ in 0..5000 {
            let r = draw(&mut rng, &so, &sp, Some(4.0));
            pits.push(normal.pit(r, &so, &sp).unwrap());
        }
        let w2 = cramer_von_mises_uniform(&pits).unwrap();
        assert!(w2 > 0.461, "mismatched W²={w2} should exceed the 5% cv");
    }

    // -- (e) the log score is proper in practice ------------------------

    fn mean_and_se(xs: &[f64]) -> (f64, f64) {
        let n = xs.len() as f64;
        let m = xs.iter().sum::<f64>() / n;
        let v = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0);
        (m, (v / n).sqrt())
    }

    #[test]
    fn log_score_is_proper_t4_beats_normal_on_t4_data() {
        let mut rng = Rng::new(0x0000_ABCD);
        let so = [[0.5, 0.05], [0.05, 0.4]];
        let sp = scale2(&so, 0.1);
        let t4 = LawQuad::student_t(4.0).unwrap();
        let normal = LawQuad::normal();
        let diff: Vec<f64> = (0..8000)
            .map(|_| {
                let r = draw(&mut rng, &so, &sp, Some(4.0));
                t4.log_density(r, &so, &sp).unwrap() - normal.log_density(r, &so, &sp).unwrap()
            })
            .collect();
        let (m, se) = mean_and_se(&diff);
        assert!(m > 3.0 * se, "t4−normal on t4 data: {m} ± {se}");
    }

    #[test]
    fn log_score_is_proper_normal_beats_t4_on_gaussian_data() {
        let mut rng = Rng::new(0x0000_BEEF);
        let so = [[0.5, 0.05], [0.05, 0.4]];
        let sp = scale2(&so, 0.1);
        let t4 = LawQuad::student_t(4.0).unwrap();
        let normal = LawQuad::normal();
        let diff: Vec<f64> = (0..8000)
            .map(|_| {
                let r = draw(&mut rng, &so, &sp, None);
                normal.log_density(r, &so, &sp).unwrap() - t4.log_density(r, &so, &sp).unwrap()
            })
            .collect();
        let (m, se) = mean_and_se(&diff);
        assert!(m > 3.0 * se, "normal−t4 on gaussian data: {m} ± {se}");
    }

    #[test]
    fn inflating_or_deflating_sigma_pred_lowers_the_mean_log_score() {
        let mut rng = Rng::new(0x0000_5EED);
        let so = [[0.5, 0.05], [0.05, 0.4]];
        let sp = scale2(&so, 1.0);
        let nu = 4.0;
        let law = LawQuad::student_t(nu).unwrap();
        let (mut truth, mut inflated, mut deflated) = (0.0, 0.0, 0.0);
        for _ in 0..20000 {
            let r = draw(&mut rng, &so, &sp, Some(nu));
            truth += law.log_density(r, &so, &sp).unwrap();
            inflated += law.log_density(r, &so, &scale2(&sp, 2.0)).unwrap();
            deflated += law.log_density(r, &so, &scale2(&sp, 0.5)).unwrap();
        }
        assert!(truth > inflated, "truth {truth} !> inflated {inflated}");
        assert!(truth > deflated, "truth {truth} !> deflated {deflated}");
    }

    // -- refusals -------------------------------------------------------

    #[test]
    fn student_t_refuses_nu_at_or_below_two() {
        assert_eq!(
            LawQuad::student_t(2.0).unwrap_err(),
            LawError::NuOutOfRange(2.0)
        );
        assert_eq!(
            LawQuad::student_t(1.5).unwrap_err(),
            LawError::NuOutOfRange(1.5)
        );
        assert!(matches!(
            LawQuad::student_t(f64::NAN).unwrap_err(),
            LawError::NuOutOfRange(_)
        ));
    }

    #[test]
    fn non_pd_combined_and_non_finite_are_distinct_errors() {
        let law = LawQuad::normal();
        let so = [[1.0, 0.0], [0.0, 1.0]];
        let bad = [[1.0, 2.0], [2.0, 1.0]]; // indefinite
        assert_eq!(
            law.score_row([0.1, 0.1], &so, &bad).unwrap_err(),
            LawError::NonPdCombined
        );
        assert_eq!(
            law.score_row([f64::NAN, 0.0], &so, &so).unwrap_err(),
            LawError::NonFiniteResidual
        );
    }

    #[test]
    fn cramer_von_mises_and_pit_histogram_basics() {
        let pits: Vec<f64> = (0..1000).map(|i| (i as f64 + 0.5) / 1000.0).collect();
        assert!(cramer_von_mises_uniform(&pits).unwrap() < 1e-3);
        let h = pit_histogram(&pits, 20);
        assert_eq!(h.iter().sum::<u32>(), 1000);
        assert!(h.iter().all(|&c| c == 50));
        assert_eq!(pit_histogram(&[1.0], 20)[19], 1);
    }
}
