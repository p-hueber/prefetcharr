use std::future::{Ready, ready};

use tracing::debug;

use crate::media_server::NowPlaying;

pub fn users(users: &[String]) -> impl FnMut(&NowPlaying) -> Ready<bool> {
    move |np: &NowPlaying| {
        let accept =
            users.is_empty() || users.contains(&np.user.id) || users.contains(&np.user.name);
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

pub fn libraries(libraries: &[String]) -> impl FnMut(&NowPlaying) -> Ready<bool> {
    move |np: &NowPlaying| {
        let library = np.library.as_ref();
        let accept = libraries.is_empty() || library.is_some_and(|l| libraries.contains(l));
        if !accept {
            debug!(
                now_playing = ?np,
                ?libraries,
                "Ignoring session from unwanted library"
            );
        }
        ready(accept)
    }
}

#[cfg(test)]
mod test {
    use crate::media_server::{NowPlaying, User};

    fn np_default() -> NowPlaying {
        NowPlaying {
            series: crate::media_server::Series::Tvdb(0),
            episode: 0,
            season: 0,
            user: User {
                name: String::new(),
                id: String::new(),
            },
            library: None,
        }
    }

    #[tokio::test]
    async fn users_unrestricted() {
        let mut filter = super::users(&[]);
        assert!(filter(&np_default()).await);
    }

    #[tokio::test]
    async fn users_accepted_by_name() {
        let users = vec!["Other".to_string(), "User".to_string()];
        let mut filter = super::users(users.as_slice());
        let np = NowPlaying {
            user: User {
                id: "1".to_string(),
                name: "User".to_string(),
            },
            ..np_default()
        };
        assert!(filter(&np).await);
    }

    #[tokio::test]
    async fn users_accepted_by_id() {
        let users = vec!["1".to_string(), "2".to_string()];
        let mut filter = super::users(users.as_slice());
        let np = NowPlaying {
            user: User {
                id: "1".to_string(),
                name: "User".to_string(),
            },
            ..np_default()
        };
        assert!(filter(&np).await);
    }

    #[tokio::test]
    async fn users_rejected() {
        let users = vec!["Nope".to_string()];
        let mut filter = super::users(users.as_slice());
        let np = NowPlaying { ..np_default() };
        assert!(!filter(&np).await);
    }

    #[tokio::test]
    async fn libraries_unrestricted() {
        let mut filter = super::libraries(&[]);
        assert!(filter(&np_default()).await);
    }

    #[tokio::test]
    async fn libraries_accepted() {
        let libraries = vec!["Movies".to_string(), "TV".to_string()];
        let mut filter = super::libraries(libraries.as_slice());
        let np = NowPlaying {
            library: Some("TV".to_string()),
            ..np_default()
        };
        assert!(filter(&np).await);
    }

    #[tokio::test]
    async fn libraries_unknown_rejected() {
        let libraries = vec!["Nope".to_string()];
        let mut filter = super::libraries(libraries.as_slice());
        let np = NowPlaying {
            library: None,
            ..np_default()
        };
        assert!(!filter(&np).await);
    }

    #[tokio::test]
    async fn libraries_rejected() {
        let libraries = vec!["TV".to_string()];
        let mut filter = super::libraries(libraries.as_slice());
        let np = NowPlaying {
            library: Some("Movies".to_string()),
            ..np_default()
        };
        assert!(!filter(&np).await);
    }
}
