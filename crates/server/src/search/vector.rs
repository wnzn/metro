//! Vector checks, encoding and exact cosine scoring.
//!
//! Vectors are L2-normalized before they are stored, so cosine similarity is
//! a dot product. They are stored as little-endian `f32` blobs.

use std::cmp::Ordering;

/// Largest embedding dimension accepted from a provider.
pub const MAX_DIMS: usize = 8_192;

/// Why a provider's embedding was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VectorError {
    Empty,
    TooLarge(usize),
    NotFinite,
    Zero,
    Mismatch { expected: usize, got: usize },
}

impl std::fmt::Display for VectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VectorError::Empty => f.write_str("the embedding model returned an empty vector"),
            VectorError::TooLarge(n) => write!(
                f,
                "the embedding model returned {n} dimensions; at most {MAX_DIMS} are supported"
            ),
            VectorError::NotFinite => {
                f.write_str("the embedding model returned a non-finite value")
            }
            VectorError::Zero => f.write_str("the embedding model returned a zero vector"),
            VectorError::Mismatch { expected, got } => write!(
                f,
                "the embedding model returned {got} dimensions where {expected} were expected; \
                 the model changed without its connection changing"
            ),
        }
    }
}

/// Check `v` and scale it to unit length.
pub fn normalize(mut v: Vec<f32>) -> Result<Vec<f32>, VectorError> {
    if v.is_empty() {
        return Err(VectorError::Empty);
    }
    if v.len() > MAX_DIMS {
        return Err(VectorError::TooLarge(v.len()));
    }
    if v.iter().any(|x| !x.is_finite()) {
        return Err(VectorError::NotFinite);
    }
    let norm = v
        .iter()
        .map(|x| f64::from(*x) * f64::from(*x))
        .sum::<f64>()
        .sqrt();
    if !(norm.is_finite() && norm > 0.0) {
        return Err(VectorError::Zero);
    }
    for x in &mut v {
        *x = (f64::from(*x) / norm) as f32;
    }
    Ok(v)
}

/// Normalize a batch whose vectors must all have the same dimension,
/// `expected` when one is already on record.
pub fn normalize_batch(
    batch: Vec<Vec<f32>>,
    expected: Option<usize>,
) -> Result<Vec<Vec<f32>>, VectorError> {
    let mut dims = expected;
    batch
        .into_iter()
        .map(|v| {
            match dims {
                Some(expected) if expected != v.len() => {
                    return Err(VectorError::Mismatch {
                        expected,
                        got: v.len(),
                    });
                }
                _ => dims = Some(v.len()),
            }
            normalize(v)
        })
        .collect()
}

pub fn encode(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Decode a stored blob of exactly `dims` floats; anything else is `None`.
pub fn decode(blob: &[u8], dims: usize) -> Option<Vec<f32>> {
    if blob.len() != dims * 4 {
        return None;
    }
    Some(
        blob.as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
    )
}

/// Dot product of two unit vectors: their cosine similarity. Accumulates in
/// `f64` so equal inputs give equal, reproducible scores.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(x, y)| f64::from(*x) * f64::from(*y))
        .sum()
}

/// Descending by score, then by `key` ascending: a total, deterministic order.
pub fn rank_cmp<K: Ord>(a: (f64, &K), b: (f64, &K)) -> Ordering {
    b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1))
}

/// Keeps the best `k` items seen, in [`rank_cmp`] order.
pub struct TopK<K: Ord, T> {
    k: usize,
    items: Vec<(f64, K, T)>,
}

impl<K: Ord, T> TopK<K, T> {
    pub fn new(k: usize) -> Self {
        Self {
            k,
            items: Vec::with_capacity(k + 1),
        }
    }

    pub fn push(&mut self, score: f64, key: K, item: T) {
        if self.k == 0 {
            return;
        }
        if self.items.len() == self.k {
            let worst = self.items.last().expect("k > 0");
            if rank_cmp((score, &key), (worst.0, &worst.1)) != Ordering::Less {
                return;
            }
            self.items.pop();
        }
        let at = self
            .items
            .partition_point(|(s, k, _)| rank_cmp((*s, k), (score, &key)) == Ordering::Less);
        self.items.insert(at, (score, key, item));
    }

    pub fn into_sorted(self) -> Vec<(f64, K, T)> {
        self.items
    }
}
