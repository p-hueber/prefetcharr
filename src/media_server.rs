pub mod emby;
pub mod jellyfin;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Series {
    Title(String),
    Tvdb(i32),
}

#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub series: Series,
    pub episode: i32,
    pub season: i32,
}
