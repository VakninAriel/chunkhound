#![forbid(unsafe_code)]

pub mod bruteforce;
pub mod fixture;
pub mod metrics;
pub mod snapshot;

/// L2-normalize a vector in place so TurboVec's raw search score (and the
/// brute-force ground truth) are both cosine similarity — comparable to
/// the existing DB's `array_cosine_similarity`.
pub fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}
