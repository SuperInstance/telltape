//! Deterministic RNG (SplitMix64 + Box–Muller). Std-only, seeded, so the
//! synthetic fixture is bit-reproducible on every machine — ground truth
//! you cannot regenerate exactly is not ground truth.

pub struct Rng {
    state: u64,
    spare: Option<f64>,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
            spare: None,
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal sample.
    pub fn next_gauss(&mut self) -> f64 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let u1 = 1.0 - self.next_f64(); // (0,1]
        let u2 = self.next_f64();
        let r = (-2.0 * u1.ln()).sqrt();
        let t = 2.0 * std::f64::consts::PI * u2;
        self.spare = Some(r * t.sin());
        r * t.cos()
    }

    /// Random unit vector in `dim` dimensions.
    pub fn unit_vec(&mut self, dim: usize) -> Vec<f32> {
        let mut v: Vec<f64> = (0..dim).map(|_| self.next_gauss()).collect();
        let n = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        for x in v.iter_mut() {
            *x /= n;
        }
        v.iter().map(|&x| x as f32).collect()
    }
}

/// Add Gaussian noise with per-coordinate `sigma` to a unit vector and
/// renormalise. For a unit vector in `dim` dims the expected cosine
/// distance afterwards is `1 - 1/sqrt(1 + dim*sigma^2)`.
pub fn perturb(rng: &mut Rng, v: &[f32], sigma: f64) -> Vec<f32> {
    let mut out: Vec<f64> = v
        .iter()
        .map(|&x| x as f64 + rng.next_gauss() * sigma)
        .collect();
    let n = out.iter().map(|x| x * x).sum::<f64>().sqrt();
    for x in out.iter_mut() {
        *x /= n;
    }
    out.iter().map(|&x| x as f32).collect()
}

/// Per-coordinate sigma that produces (in expectation) cosine distance
/// `target` after [`perturb`]: from `1 - d = 1/sqrt(1 + dim*sigma^2)`.
pub fn sigma_for_distance(dim: usize, target: f64) -> f64 {
    assert!(target > 0.0 && target < 1.0);
    let x = (1.0 / (1.0 - target)).powi(2) - 1.0;
    (x / dim as f64).sqrt()
}

/// Why an embedding is degenerate — unable to carry a score —
/// or `None` when it is sound. Degenerate means: a non-finite
/// component, a non-finite norm, or a zero norm. This is the
/// one degeneracy rule for the whole tool (playtest F1): the
/// ingest's startup probe checks it in Python, and the cascade
/// refuses any record that trips it in Rust, so no adapter's
/// output can reach a score unexamined.
pub fn degenerate_reason(emb: &[f32]) -> Option<&'static str> {
    let mut norm_sq = 0.0f64;
    for &x in emb {
        if !x.is_finite() {
            return Some("non-finite component");
        }
        norm_sq += (x as f64) * (x as f64);
    }
    if !norm_sq.is_finite() {
        return Some("non-finite norm");
    }
    if norm_sq == 0.0 {
        return Some("zero norm");
    }
    None
}

/// Cosine distance `1 - cos(a, b)` in f64. Embeddings are NOT assumed
/// normalised (I-JEPA outputs usually are, but the pipeline must not
/// silently rely on it).
///
/// Returns `None` — *no score* — when either vector is degenerate
/// (see [`degenerate_reason`]). This is defence in depth, pinned
/// by the cold playtest (F1): the previous semantics returned 1.0
/// — maximal distance — for a zero-norm vector, so a broken
/// embedder returning zeros scored every nominated tile as
/// maximally changed and produced a perfect-looking, wrong
/// ledger. A tile with no score behaves exactly like an
/// unbaselined tile (it makes no change claim), which is the
/// honest answer to a vector that carries no information. The
/// cascade validates records before scoring and refuses
/// degenerate ones outright; this `None` is the last line for
/// direct callers of this function.
pub fn cosine_distance(a: &[f32], b: &[f32]) -> Option<f64> {
    assert_eq!(a.len(), b.len());
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..a.len() {
        let (x, y) = (a[i] as f64, b[i] as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if degenerate_reason(a).is_some() || degenerate_reason(b).is_some() {
        return None;
    }
    let d = 1.0 - dot / (na.sqrt() * nb.sqrt());
    if d.is_finite() {
        Some(d)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_distance_scores_sound_vectors() {
        let a = [1.0f32, 0.0, 0.0];
        let b = [0.0f32, 1.0, 0.0];
        assert_eq!(cosine_distance(&a, &b), Some(1.0), "orthogonal");
        assert_eq!(cosine_distance(&a, &a), Some(0.0), "identical");
        // Unnormalised inputs are fine: direction is what scores.
        let c = [2.0f32, 0.0, 0.0];
        assert_eq!(cosine_distance(&a, &c), Some(0.0));
    }

    /// Playtest F1, pinned at the scorer: a zero-norm vector used
    /// to score maximal distance (1.0) *by construction*. It must
    /// now score nothing at all — no claim, in either argument
    /// position — and so must NaN / infinite components.
    #[test]
    fn degenerate_vectors_score_nothing_never_maximal() {
        let sound = [0.6f32, 0.8, 0.0];
        let zero = [0.0f32, 0.0, 0.0];
        assert_eq!(cosine_distance(&zero, &sound), None);
        assert_eq!(cosine_distance(&sound, &zero), None);
        assert_eq!(cosine_distance(&zero, &zero), None);
        let nan = [f32::NAN, 0.0, 0.0];
        assert_eq!(cosine_distance(&nan, &sound), None);
        assert_eq!(cosine_distance(&sound, &nan), None);
        let inf = [f32::INFINITY, 0.0, 0.0];
        assert_eq!(cosine_distance(&inf, &sound), None);
        // The rule names its reasons.
        assert_eq!(degenerate_reason(&zero), Some("zero norm"));
        assert_eq!(degenerate_reason(&nan), Some("non-finite component"));
        assert_eq!(degenerate_reason(&sound), None);
        assert_eq!(degenerate_reason(&[]), Some("zero norm"));
    }
}
