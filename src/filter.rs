use std::future::{ready, Ready};

use tracing::debug;

use crate::media_server::NowPlaying;

pub fn users(users: &[String]) -> impl FnMut(&NowPlaying) -> Ready<bool> + use<'_> {
    move |np: &NowPlaying| {
        let accept =
            users.is_empty() || users.contains(&np.user_id) || users.contains(&np.user_name);
        if !accept {
            debug!(
                now_playing = ?np,
                ?users,
                "Ignoring session from unwanted user"
            );
        }
        ready(accept)
    }
}

#[cfg(test)]
mod test {
    use crate::media_server::NowPlaying;

    #[tokio::test]
    async fn users_unrestricted() {
        let mut filter = super::users(&[]);
        let np = NowPlaying {
            series: crate::media_server::Series::Tvdb(0),
            episode: 0,
            season: 0,
            user_id: "1".to_string(),
            user_name: "User".to_string(),
        };
        assert!(filter(&np).await);
    }

    #[tokio::test]
    async fn users_accepted_by_name() {
        let users = vec!["Other".to_string(), "User".to_string()];
        let mut filter = super::users(users.as_slice());
        let np = NowPlaying {
            series: crate::media_server::Series::Tvdb(0),
            episode: 0,
            season: 0,
            user_id: "1".to_string(),
            user_name: "User".to_string(),
        };
        assert!(filter(&np).await);
    }

    #[tokio::test]
    async fn users_accepted_by_id() {
        let users = vec!["1".to_string(), "2".to_string()];
        let mut filter = super::users(users.as_slice());
        let np = NowPlaying {
            series: crate::media_server::Series::Tvdb(0),
            episode: 0,
            season: 0,
            user_id: "1".to_string(),
            user_name: "User".to_string(),
        };
        assert!(filter(&np).await);
    }

    #[tokio::test]
    async fn users_rejected() {
        let users = vec!["Nope".to_string()];
        let mut filter = super::users(users.as_slice());
        let np = NowPlaying {
            series: crate::media_server::Series::Tvdb(0),
            episode: 0,
            season: 0,
            user_id: "1".to_string(),
            user_name: "User".to_string(),
        };
        assert!(!filter(&np).await);
    }
}
