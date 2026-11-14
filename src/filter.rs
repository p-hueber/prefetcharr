use std::future::{Ready, ready};

use tracing::{debug, warn};

use crate::media_server::NowPlaying;

pub fn users(
    users: &[String],
    excluded_users: &[String],
) -> impl FnMut(&NowPlaying) -> Ready<bool> {
    move |np: &NowPlaying| {
        let matches_include = users.contains(&np.user.id) || users.contains(&np.user.name);
        let included = users.is_empty() || matches_include;
        let excluded =
            excluded_users.contains(&np.user.id) || excluded_users.contains(&np.user.name);
        if excluded {
            if matches_include {
                warn!(
                    now_playing = ?np,
                    ?excluded_users,
                    "Ignoring session because user matched both include and exclude lists"
                );
            }
            return ready(false);
        }

        if !included {
            debug!(
                now_playing = ?np,
                ?users,
                "Ignoring session from unwanted user"
            );
        }
        ready(included)
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
            session_id: None,
        }
    }

    // Empty include and exclude lists accept all sessions
    #[tokio::test]
    async fn users_unrestricted() {
        let mut filter = super::users(&[], &[]);
        assert!(filter(&np_default()).await);
    }

    // Sessions matching a configured user name are accepted
    #[tokio::test]
    async fn users_accepted_by_name() {
        let users = vec!["Other".to_string(), "User".to_string()];
        let mut filter = super::users(users.as_slice(), &[]);
        let np = NowPlaying {
            user: User {
                id: "1".to_string(),
                name: "User".to_string(),
            },
            ..np_default()
        };
        assert!(filter(&np).await);
    }

    // Sessions matching a configured user ID are accepted
    #[tokio::test]
    async fn users_accepted_by_id() {
        let users = vec!["1".to_string(), "2".to_string()];
        let mut filter = super::users(users.as_slice(), &[]);
        let np = NowPlaying {
            user: User {
                id: "1".to_string(),
                name: "User".to_string(),
            },
            ..np_default()
        };
        assert!(filter(&np).await);
    }

    // Sessions not matching any configured user are rejected
    #[tokio::test]
    async fn users_rejected() {
        let users = vec!["Nope".to_string()];
        let mut filter = super::users(users.as_slice(), &[]);
        let np = NowPlaying { ..np_default() };
        assert!(!filter(&np).await);
    }

    #[tokio::test]
    async fn users_excluded_by_name() {
        let excluded_users = vec!["User".to_string()];
        let mut filter = super::users(&[], excluded_users.as_slice());
        let np = NowPlaying {
            user: User {
                id: "1".to_string(),
                name: "User".to_string(),
            },
            ..np_default()
        };
        assert!(!filter(&np).await);
    }

    #[tokio::test]
    async fn users_excluded_by_id() {
        let excluded_users = vec!["1".to_string()];
        let mut filter = super::users(&[], excluded_users.as_slice());
        let np = NowPlaying {
            user: User {
                id: "1".to_string(),
                name: "User".to_string(),
            },
            ..np_default()
        };
        assert!(!filter(&np).await);
    }

    #[tokio::test]
    async fn users_excluded_even_when_included() {
        let users = vec!["User".to_string()];
        let excluded_users = vec!["User".to_string()];
        let mut filter = super::users(users.as_slice(), excluded_users.as_slice());
        let np = NowPlaying {
            user: User {
                id: "1".to_string(),
                name: "User".to_string(),
            },
            ..np_default()
        };
        assert!(!filter(&np).await);
    }

    #[tokio::test]
    async fn exclusions_apply_when_users_are_unrestricted() {
        let excluded_users = vec!["User".to_string()];
        let mut filter = super::users(&[], excluded_users.as_slice());
        let excluded = NowPlaying {
            user: User {
                id: "1".to_string(),
                name: "User".to_string(),
            },
            ..np_default()
        };
        let included = NowPlaying {
            user: User {
                id: "2".to_string(),
                name: "Other".to_string(),
            },
            ..np_default()
        };
        assert!(!filter(&excluded).await);
        assert!(filter(&included).await);
    }

    // Empty library list accepts all sessions
    #[tokio::test]
    async fn libraries_unrestricted() {
        let mut filter = super::libraries(&[]);
        assert!(filter(&np_default()).await);
    }

    // Sessions from a matching library are accepted
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

    // Sessions with no library set are rejected when a library filter is active
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

    // Sessions from a non-matching library are rejected
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
