# prefetcharr #

Let [Sonarr](https://sonarr.tv) fetch the next season of a show you are watching
on [Jellyfin](https://jellyfin.org).

## Details ##

_prefetcharr_ periodically polls _Jellyfin_ for active playback sessions. For TV
shows, it checks whether the episode played is near the end of a season. If
that's the case and the next season isn't yet monitored on _Sonarr_, it starts
monitoring it and initiates a season search.

## Build and install ##

[Install Rust](https://www.rust-lang.org/tools/install), clone this repo and run
```
cargo install --lock --path .
```
