//! Deterministic bag-of-words encoder for tests and examples: hashes lowercase word pieces into
//! a fixed number of buckets. No model weights needed.

use crate::engine::Encoder;

/// Hashing encoder. Documents and queries share one space, so shared words score high.
#[derive(Debug, Clone)]
pub struct HashEncoder {
    pub dim: usize,
    pub calls: usize,
}

impl HashEncoder {
    pub fn new(dim: usize) -> Self {
        Self { dim, calls: 0 }
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        for word in text
            .split(|c: char| !c.is_alphanumeric())
            .flat_map(|w| w.split('_'))
            .filter(|w| w.len() >= 3)
        {
            let h = word
                .to_lowercase()
                .bytes()
                .fold(1469598103934665603u64, |h, b| {
                    (h ^ b as u64).wrapping_mul(1099511628211)
                });
            v[(h % self.dim as u64) as usize] += 1.0;
        }
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 {
            v.iter_mut().for_each(|x| *x /= n);
        }
        v
    }
}

impl Encoder for HashEncoder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn fingerprint(&self) -> String {
        format!("hash-{}", self.dim)
    }

    fn name(&self) -> String {
        "hash-encoder (test)".into()
    }

    fn encode_documents(&mut self, docs: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.calls += docs.len();
        Ok(docs.iter().map(|d| self.embed(d)).collect())
    }

    fn encode_query(&mut self, query: &str, context: &str) -> Result<Vec<f32>, String> {
        Ok(self.embed(&format!("{query} {context}")))
    }
}
