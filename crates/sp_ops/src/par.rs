//! Minimal data parallelism for per-formula work (parse, shift, print) over big sheets.

/// `items.iter().map(f).collect()`, split across the available cores when there is enough
/// work. Order is preserved. A panicking worker's chunk is recomputed on the caller's
/// thread (so a bug surfaces as it would serially, never as lost results).
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    /// Below this, threads cost more than they save.
    const MIN_PARALLEL: usize = 4_096;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(16));
    if threads < 2 || items.len() < MIN_PARALLEL {
        return items.iter().map(&f).collect();
    }
    let per = items.len().div_ceil(threads);
    let f = &f;
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(per)
            .map(|chunk| scope.spawn(move || chunk.iter().map(f).collect::<Vec<R>>()))
            .collect();
        let mut out = Vec::with_capacity(items.len());
        for (h, chunk) in handles.into_iter().zip(items.chunks(per)) {
            match h.join() {
                Ok(v) => out.extend(v),
                Err(_) => out.extend(chunk.iter().map(f)),
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_order_small_and_large() {
        let small: Vec<u32> = (0..10).collect();
        assert_eq!(
            par_map(&small, |x| x * 2),
            (0..10).map(|x| x * 2).collect::<Vec<_>>()
        );
        let big: Vec<u64> = (0..100_003).collect();
        let out = par_map(&big, |x| x.to_string());
        assert_eq!(out.len(), big.len());
        assert!(out.iter().zip(&big).all(|(s, x)| *s == x.to_string()));
    }
}
