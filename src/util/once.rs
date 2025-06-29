use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use crate::media_server::NowPlaying;

const RETAIN_DURATION: Duration = Duration::from_secs(60 * 60 * 24 * 7);

#[derive(Default)]
pub struct Seen(HashMap<NowPlaying, Instant>);

impl Seen {
    pub fn once(&mut self, np: NowPlaying) -> bool {
        self.prune();
        self.0.insert(np, Instant::now()).is_none()
    }

    fn prune(&mut self) {
        self.0
            .retain(|_, t| Instant::now().saturating_duration_since(*t) <= RETAIN_DURATION);
    }
}

#[cfg(test)]
mod test {
    use std::time::{Duration, Instant};

    use crate::{
        media_server::{NowPlaying, Series, User},
        util::once::Seen,
    };

    fn now_playing() -> NowPlaying {
        let series = Series::Tvdb(1);
        let season = 3;
        let episode = 1;
        let user = User {
            name: "Hans".to_string(),
            id: "1234".to_string(),
        };
        let library = None;

        NowPlaying {
            series,
            episode,
            season,
            user,
            library,
        }
    }
    #[test]

    fn twice() {
        let mut seen = Seen::default();
        let np = now_playing();
        assert!(seen.once(np.clone()));
        assert!(!seen.once(np));
    }

    #[test]
    fn prune_old() {
        let mut seen = Seen::default();
        let np = now_playing();
        let touched = Instant::now().checked_sub(super::RETAIN_DURATION).unwrap();

        seen.0.insert(np.clone(), touched);
        assert!(seen.once(np));
    }

    #[test]
    fn touch() {
        let mut seen = Seen::default();
        let np = now_playing();

        // let mut old = Entry::new(series.clone(), season);
        let touched = (Instant::now() + Duration::from_millis(100))
            .checked_sub(super::RETAIN_DURATION)
            .unwrap();

        seen.0.insert(np.clone(), touched);
        assert!(!seen.once(np.clone()));

        std::thread::sleep(Duration::from_millis(100));
        assert!(!seen.once(np));
    }

    #[test]
    fn different_season() {
        let mut seen = Seen::default();
        let np_a = now_playing();
        let np_b = {
            let mut np = now_playing();
            np.season += 1;
            np
        };
        assert!(seen.once(np_a));
        assert!(seen.once(np_b));
    }
}
