# prefetcharr #

Let [Sonarr](https://sonarr.tv) fetch the next season of a show you are watching
on [Jellyfin](https://jellyfin.org).

## Details ##

_prefetcharr_ periodically polls _Jellyfin_ for active playback sessions. For
TV shows, it checks whether the episode played is near the end of a season. If
that's the case and the next season isn't downloaded yet, _prefetcharr_ asks
_Sonarr_ to monitor it and initiate a season search.

## Build and install ##

[Install Rust](https://www.rust-lang.org/tools/install), clone this repo and run
```
cargo install --lock --path .
```

Or with docker compose:
```yml
version: '3.5'
services:
  prefetcharr:
    build: https://github.com/p-hueber/prefetcharr.git
    image: prefetcharr
    environment:
      - JELLYFIN_URL=http://example.com/jellyfin
      - JELLYFIN_API_KEY=<YOUR KEY HERE>
      - SONARR_URL=http://example.com/sonarr
      - SONARR_API_KEY=<YOUR KEY HERE>
      - LOG_DIR=/log
      - RUST_LOG=prefetcharr=debug
    volumes:
      - /path/to/log/dir:/log

```
