pub mod emby;
pub mod jellyfin;

#[derive(Debug)]
pub struct NowPlaying {
    pub tvdb: i32,
    pub episode: i32,
    pub season: i32,
}
