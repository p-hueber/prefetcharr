use std::{
    collections::HashSet,
    hash::Hash,
    time::{Duration, Instant},
};

use crate::media_server::Series;

const RETAIN_DURATION: Duration = Duration::from_secs(60 * 60 * 24 * 7);

#[derive(PartialEq, Eq, Hash)]
struct Season {
    series: Series,
    season: i32,
}

struct Entry {
    season: Season,
    touched: Instant,
}

impl Entry {
    fn new(series: Series, season: i32) -> Self {
        Self {
            season: Season { series, season },
            touched: Instant::now(),
        }
    }
}

impl Eq for Entry {}

impl PartialEq for Entry {
    fn eq(&self, other: &Entry) -> bool {
        <Season as PartialEq>::eq(&self.season, &other.season)
    }
}

impl Hash for Entry {
    fn hash<H: core::hash::Hasher>(&self, ra_expand_state: &mut H) {
        <Season as Hash>::hash(&self.season, ra_expand_state);
    }
}

#[derive(Default)]
pub struct Seen(HashSet<Entry>);

impl Seen {
    pub fn once(&mut self, series: Series, season: i32) -> bool {
        self.prune();
        self.0.replace(Entry::new(series, season)).is_none()
    }

    fn prune(&mut self) {
        self.0
            .retain(|e| Instant::now().saturating_duration_since(e.touched) <= RETAIN_DURATION);
    }
}

#[cfg(test)]
mod test {
    use std::time::{Duration, Instant};

    use crate::{
        media_server::Series,
        util::once::{Entry, Seen},
    };

    #[test]
    fn twice() {
        let mut seen = Seen::default();
        let series = Series::Tvdb(1);
        let season = 3;
        assert!(seen.once(series.clone(), season));
        assert!(!seen.once(series, season));
    }

    #[test]
    fn prune_old() {
        let mut seen = Seen::default();
        let series = Series::Tvdb(1);
        let season = 3;

        let mut old = Entry::new(series.clone(), season);
        old.touched = Instant::now().checked_sub(super::RETAIN_DURATION).unwrap();

        seen.0.replace(old);
        assert!(seen.once(series, season));
    }

    #[test]
    fn touch() {
        let mut seen = Seen::default();
        let series = Series::Tvdb(1);
        let season = 3;

        let mut old = Entry::new(series.clone(), season);
        old.touched = (Instant::now() + Duration::from_millis(100))
            .checked_sub(super::RETAIN_DURATION)
            .unwrap();

        seen.0.replace(old);
        assert!(!seen.once(series.clone(), season));

        std::thread::sleep(Duration::from_millis(100));
        assert!(!seen.once(series, season));
    }

    #[test]
    fn different_season() {
        let mut seen = Seen::default();
        let series = Series::Tvdb(1);
        assert!(seen.once(series.clone(), 1));
        assert!(seen.once(series, 2));
    }

    #[test]
    fn different_series() {
        let mut seen = Seen::default();
        let season = 1;
        assert!(seen.once(Series::Tvdb(1), season));
        assert!(seen.once(Series::Tvdb(2), season));
    }
}
