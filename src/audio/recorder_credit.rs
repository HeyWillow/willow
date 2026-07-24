//! Scheduling policy for sequential ESP-SR feed and fetch operations.

/// Reports whether one output frame can be fetched while retaining the input
/// lead required by ESP-SR's smaller internal processing blocks.
pub(super) const fn fetch_ready(
    credited_samples: usize,
    lead_samples: usize,
    fetch_samples: usize,
) -> bool {
    credited_samples.saturating_sub(lead_samples) >= fetch_samples
}

#[cfg(test)]
mod tests {
    #[test]
    fn retains_one_feed_frame_before_the_first_fetch() {
        let feed_samples = 512;
        let fetch_samples = 512;

        assert!(!super::fetch_ready(
            feed_samples,
            feed_samples,
            fetch_samples
        ));
        assert!(super::fetch_ready(
            feed_samples * 2,
            feed_samples,
            fetch_samples
        ));
    }

    #[test]
    fn preserves_the_lead_during_steady_capture() {
        let feed_samples = 512;
        let fetch_samples = 512;
        let mut credit = feed_samples * 2;

        for _ in 0..32 {
            assert!(super::fetch_ready(credit, feed_samples, fetch_samples));
            credit -= fetch_samples;
            assert!(!super::fetch_ready(credit, feed_samples, fetch_samples));
            credit += feed_samples;
        }
    }

    #[test]
    fn lead_covers_160_sample_internal_output_blocks() {
        let feed_samples = 512;
        let fetch_samples = 512;
        let mut credited = 0;
        let mut fed = 0;
        let mut fetched = 0;

        for _ in 0..64 {
            credited += feed_samples;
            fed += feed_samples;
            let processed = fed / 160 * 160;

            while super::fetch_ready(credited, feed_samples, fetch_samples) {
                assert!(processed - fetched >= fetch_samples);
                credited -= fetch_samples;
                fetched += fetch_samples;
            }
        }
    }
}
