//! The personal adapter: a query-side linear map learned from a repository's own history.
//!
//! Document vectors stay frozen (so fitting never forces a re-index). For each past commit the
//! query is the commit message and the positive is a file it changed; the map `W` (identity
//! init) minimises InfoNCE over the positive and sampled negatives plus `reg * ||W - I||²`, with
//! Adam. This is the same objective, initialisation and optimiser as the evaluated reference, and
//! golden tests pin the weights it produces.

use half::f16;
use ndarray::ArrayView2;

/// Hyper-parameters of the adapter fit (defaults are the evaluated ones).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdapterParams {
    /// Adam steps.
    pub steps: usize,
    /// Adam learning rate.
    pub lr: f32,
    /// Strength of the pull back towards the identity map.
    pub reg: f32,
    /// InfoNCE temperature.
    pub tau: f32,
    /// Negatives sampled per example.
    pub negatives: usize,
}

impl Default for AdapterParams {
    fn default() -> Self {
        Self {
            steps: 80,
            lr: 1e-3,
            reg: 0.1,
            tau: 0.05,
            negatives: 64,
        }
    }
}

/// Newest commits used to fit an adapter.
pub const ADAPTER_COMMITS: usize = 200;
/// Fewer usable examples than this and no adapter is fitted (identity fallback).
pub const ADAPTER_MIN_TRAIN: usize = 20;
/// Refit when this many new commits landed since the adapter's history cutoff.
pub const ADAPTER_REFIT_EVERY: usize = 50;

/// `c[m×n] = a[m×k] · b[k×n]` for row-major matrices, with optional transposition of `a`
/// (stored as `k×m`). Uses ndarray's safe matrix product (matrixmultiply underneath).
fn gemm(a: &[f32], a_t: bool, b: &[f32], m: usize, k: usize, n: usize, c: &mut [f32]) {
    let a = if a_t {
        ArrayView2::from_shape((k, m), &a[..k * m])
            .expect("shape")
            .reversed_axes()
    } else {
        ArrayView2::from_shape((m, k), &a[..m * k]).expect("shape")
    };
    let b = ArrayView2::from_shape((k, n), &b[..k * n]).expect("shape");
    let out = a.dot(&b);
    for (dst, src) in c[..m * n].iter_mut().zip(out.iter()) {
        *dst = *src;
    }
}

/// Fits the adapter. `qv` is `n×d` query vectors, `cand` is `m×d` document vectors (both
/// row-major), `pos[i]` is the positive document for query `i` and `negs[i]` its negatives.
/// Returns `W` as a row-major `d×d` matrix; queries are adapted as `normalize(q · W)`.
pub fn fit_adapter(
    qv: &[f32],
    cand: &[f32],
    d: usize,
    pos: &[usize],
    negs: &[Vec<usize>],
    params: &AdapterParams,
) -> Vec<f32> {
    let n = pos.len();
    assert_eq!(qv.len(), n * d, "qv must be n×d");
    assert_eq!(negs.len(), n, "one negative list per query");
    let mut w = identity(d);
    if n == 0 {
        return w;
    }
    let width = 1 + negs.iter().map(Vec::len).max().unwrap_or(0);
    // Candidates per example: positive first, then negatives, padded (and masked) with the positive.
    let mut ids = vec![0usize; n * width];
    let mut mask = vec![false; n * width];
    for i in 0..n {
        ids[i * width] = pos[i];
        mask[i * width] = true;
        for j in 1..width {
            let neg = negs[i].get(j - 1);
            ids[i * width + j] = *neg.unwrap_or(&pos[i]);
            mask[i * width + j] = neg.is_some();
        }
    }
    let c = |i: usize, j: usize| -> &[f32] {
        let id = ids[i * width + j];
        &cand[id * d..(id + 1) * d]
    };
    let eye = identity(d);
    let mut m1 = vec![0f32; d * d];
    let mut m2 = vec![0f32; d * d];
    let (b1, b2, eps) = (0.9f32, 0.999f32, 1e-8f32);
    let mut z = vec![0f32; n * d];
    let mut u = vec![0f32; n * d];
    let mut norm = vec![0f32; n];
    let mut dz = vec![0f32; n * d];
    let mut g = vec![0f32; d * d];
    let mut p = vec![0f32; width];
    let mut du = vec![0f32; d];
    let inv_tau = 1.0 / params.tau;
    for t in 1..=params.steps {
        gemm(qv, false, &w, n, d, d, &mut z);
        for i in 0..n {
            let row = &z[i * d..(i + 1) * d];
            let nr = row.iter().map(|x| x * x).sum::<f32>().sqrt();
            norm[i] = nr;
            for k in 0..d {
                u[i * d + k] = row[k] / nr;
            }
        }
        for i in 0..n {
            let ui = &u[i * d..(i + 1) * d];
            let mut max = f32::NEG_INFINITY;
            for (j, pj) in p.iter_mut().enumerate() {
                let logit = if mask[i * width + j] {
                    c(i, j).iter().zip(ui).map(|(a, b)| a * b).sum::<f32>() * inv_tau
                } else {
                    -1e9
                };
                *pj = logit;
                max = max.max(logit);
            }
            let mut sum = 0f32;
            for pj in p.iter_mut() {
                *pj = (*pj - max).exp();
                sum += *pj;
            }
            for pj in p.iter_mut() {
                *pj /= sum;
            }
            p[0] -= 1.0;
            du.iter_mut().for_each(|x| *x = 0.0);
            for (j, pj) in p.iter().enumerate() {
                let coef = pj / n as f32;
                if coef != 0.0 {
                    for (acc, cv) in du.iter_mut().zip(c(i, j)) {
                        *acc += coef * cv;
                    }
                }
            }
            du.iter_mut().for_each(|x| *x *= inv_tau);
            let dot: f32 = ui.iter().zip(&du).map(|(a, b)| a * b).sum();
            for k in 0..d {
                dz[i * d + k] = (du[k] - ui[k] * dot) / norm[i];
            }
        }
        gemm(qv, true, &dz, d, n, d, &mut g);
        let c1 = (1.0 - (b1 as f64).powi(t as i32)) as f32;
        let c2 = (1.0 - (b2 as f64).powi(t as i32)) as f32;
        for k in 0..d * d {
            let gk = g[k] + 2.0 * params.reg * (w[k] - eye[k]);
            m1[k] = b1 * m1[k] + (1.0 - b1) * gk;
            m2[k] = b2 * m2[k] + (1.0 - b2) * gk * gk;
            w[k] -= params.lr * (m1[k] / c1) / ((m2[k] / c2).sqrt() + eps);
        }
    }
    w
}

fn identity(d: usize) -> Vec<f32> {
    let mut w = vec![0f32; d * d];
    for k in 0..d {
        w[k * d + k] = 1.0;
    }
    w
}

/// Adapts a query vector: `normalize(q · W)`.
pub fn apply_adapter(q: &[f32], w: &[f32], d: usize) -> Vec<f32> {
    assert_eq!(q.len(), d);
    assert_eq!(w.len(), d * d);
    let mut out = vec![0f32; d];
    gemm(q, false, w, 1, d, d, &mut out);
    let nr = out.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-8);
    out.iter_mut().for_each(|x| *x /= nr);
    out
}

/// Short content revision of an adapter: SHA-1 of its float16 weights, first 12 hex characters
/// (the same identifier the reference writes to the adapter metadata).
pub fn revision(w: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(w.len() * 2);
    for x in w {
        bytes.extend_from_slice(&f16::from_f32(*x).to_le_bytes());
    }
    let hex = sha1_smol::Sha1::from(&bytes).digest().to_string();
    hex[..12].to_string()
}

/// Deterministic negative sampling: for each positive, up to `k` distinct other documents out of
/// `m`, chosen with a seeded generator (so refits are reproducible).
pub fn sample_negatives(pos: &[usize], m: usize, k: usize, seed: u64) -> Vec<Vec<usize>> {
    let mut rng = SplitMix64(seed);
    pos.iter()
        .map(|&p| {
            let mut others: Vec<usize> = (0..m).filter(|&x| x != p).collect();
            let take = k.min(others.len());
            // Partial Fisher-Yates: the first `take` slots become a uniform sample.
            for i in 0..take {
                let j = i + (rng.next() % (others.len() - i) as u64) as usize;
                others.swap(i, j);
            }
            others.truncate(take);
            others
        })
        .collect()
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_steps_is_identity() {
        let params = AdapterParams {
            steps: 0,
            ..AdapterParams::default()
        };
        let w = fit_adapter(
            &[1.0, 0.0],
            &[1.0, 0.0, 0.0, 1.0],
            2,
            &[0],
            &[vec![1]],
            &params,
        );
        assert_eq!(w, identity(2));
    }

    #[test]
    fn negatives_exclude_the_positive_and_are_distinct() {
        let negs = sample_negatives(&[3, 0, 7], 10, 5, 1);
        for (p, ns) in [3, 0, 7].iter().zip(&negs) {
            assert_eq!(ns.len(), 5);
            assert!(!ns.contains(p));
            let mut s = ns.clone();
            s.sort_unstable();
            s.dedup();
            assert_eq!(s.len(), 5);
        }
        assert_eq!(negs, sample_negatives(&[3, 0, 7], 10, 5, 1));
    }
}
