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

/// Cosine distance `1 - cos(a, b)` in f64. Embeddings are NOT assumed
/// normalised (I-JEPA outputs usually are, but the pipeline must not
/// silently rely on it).
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f64 {
    assert_eq!(a.len(), b.len());
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..a.len() {
        let (x, y) = (a[i] as f64, b[i] as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 1.0;
    }
    1.0 - dot / (na.sqrt() * nb.sqrt())
}
