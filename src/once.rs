use std::{
    collections::HashSet,
    hash::Hash,
    time::{Duration, Instant},
};

use crate::jellyfin::NowPlaying;

const RETAIN_DURATION: Duration = Duration::from_secs(60 * 60 * 24 * 7);

#[derive(PartialEq, Eq, Hash)]
struct Season {
    tvdb: i32,
    season: i32,
}

pub struct Entry {
    season: Season,
    touched: Instant,
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

impl From<&NowPlaying> for Entry {
    fn from(value: &NowPlaying) -> Self {
        Self {
            season: Season {
                tvdb: value.tvdb,
                season: value.season,
            },
            touched: Instant::now(),
        }
    }
}

#[derive(Default)]
pub struct Seen(HashSet<Entry>);

impl Seen {
    pub fn once<T>(&mut self, season: T) -> bool
    where
        Entry: From<T>,
    {
        self.prune();
        self.0.replace(season.into()).is_none()
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
        jellyfin::NowPlaying,
        once::{Entry, Seen},
    };

    #[test]
    fn twice() {
        let mut seen = Seen::default();
        let np = NowPlaying {
            tvdb: 1,
            episode: 2,
            season: 3,
        };
        assert!(seen.once(&np));
        assert!(!seen.once(&np));
    }

    #[test]
    fn prune_old() {
        let mut seen = Seen::default();
        let np = NowPlaying {
            tvdb: 1,
            episode: 2,
            season: 3,
        };

        let mut old = Entry::from(&np);
        old.touched = Instant::now().checked_sub(super::RETAIN_DURATION).unwrap();

        assert!(seen.once(old));
        assert!(seen.once(&np));
    }

    #[test]
    fn touch() {
        let mut seen = Seen::default();
        let np = NowPlaying {
            tvdb: 1,
            episode: 2,
            season: 3,
        };

        let mut old = Entry::from(&np);
        old.touched = (Instant::now() + Duration::from_millis(100))
            .checked_sub(super::RETAIN_DURATION)
            .unwrap();

        assert!(seen.once(old));
        assert!(!seen.once(&np));

        std::thread::sleep(Duration::from_millis(100));
        assert!(!seen.once(&np));
    }

    #[test]
    fn ignore_episode() {
        let mut seen = Seen::default();
        let np1 = NowPlaying {
            tvdb: 1,
            episode: 2,
            season: 3,
        };
        let np2 = NowPlaying {
            tvdb: 1,
            episode: 4,
            season: 3,
        };
        assert!(seen.once(&np1));
        assert!(!seen.once(&np2));
    }

    #[test]
    fn different_season() {
        let mut seen = Seen::default();
        let np1 = NowPlaying {
            tvdb: 1,
            episode: 1,
            season: 1,
        };
        let np2 = NowPlaying {
            tvdb: 1,
            episode: 1,
            season: 2,
        };
        assert!(seen.once(&np1));
        assert!(seen.once(&np2));
    }

    #[test]
    fn different_series() {
        let mut seen = Seen::default();
        let np1 = NowPlaying {
            tvdb: 1,
            episode: 1,
            season: 1,
        };
        let np2 = NowPlaying {
            tvdb: 2,
            episode: 1,
            season: 1,
        };
        assert!(seen.once(&np1));
        assert!(seen.once(&np2));
    }
}
