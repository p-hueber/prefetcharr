# prefetcharr

Let [Sonarr](https://sonarr.tv) fetch the next season of a show you are watching
on [Jellyfin](https://jellyfin.org)/[Emby](https://emby.media)/[Plex](https://www.plex.tv).

## Details

_prefetcharr_ periodically polls your media server for active playback sessions.
For TV shows, it checks whether the pilot is playing or if the end of a season
is almost reached.  
If this is the case and the next/first season has not been downloaded yet,
_prefetcharr_ asks _Sonarr_ to monitor it and initiate a season search. If there
are no more seasons left, the series gets monitored for new seasons instead.

## Build and install

To install, first ensure Rust is installed on your system by following the
instructions at [Install Rust](https://www.rust-lang.org/tools/install), then
run:
```
cargo install --git https://github.com/p-hueber/prefetcharr
```

Or with docker compose:
```yml
version: '3.5'
services:
  prefetcharr:
    image: phueber/prefetcharr:latest
    container_name: prefetcharr
    environment:
      # `jellyfin`, `emby` or `plex`
      - MEDIA_SERVER_TYPE=jellyfin
      # Jellyfin/Emby/Plex baseurl
      - MEDIA_SERVER_URL=http://example.com/jellyfin
      # Jellyfin/Emby API key or plex server token
      - MEDIA_SERVER_API_KEY=<YOUR KEY HERE>
      # Sonarr baseurl
      - SONARR_URL=http://example.com/sonarr
      # Sonarr API key
      - SONARR_API_KEY=<YOUR KEY HERE>
      # Logging directory
      - LOG_DIR=/log
      # Log level
      - RUST_LOG=prefetcharr=debug
      # Polling interval in seconds
      - INTERVAL=900
      # The last <NUM> episodes trigger a search
      - REMAINING_EPISODES=2
    volumes:
      - /path/to/log/dir:/log

```
## Configuration

### API keys

_prefetcharr_ needs two different API keys to do its job.

#### `SONARR_API_KEY`

Go to `Settings` -> `General` -> `Security` and copy the API key.

#### `MEDIA_SERVER_API_KEY`

The key to use and how to obtain it differs on the type of media server you use:

#### Jellyfin

Log in as an administrator and go to `Administration` -> `Dashboard` ->
`Advanced` -> `Api Keys`. Add a new key or use an existing one.

#### Emby

Log in as an administrator, click on the gear on the top right and go to
`Advanced` -> `Api Keys`. Add a new key or use an existing one.

#### Plex

You need to [extract the server token](https://www.plexopedia.com/plex-media-server/general/plex-token/#plexservertoken)
from a configuration file and use it as the API key.

### Upgrading pilots

If you want to store pilot episodes only, _prefetcharr_ can fetch the first
season for you on demand.  
This method works well for individual episodes but may encounter issues with
season packs.  
For this to function in _Sonarr_, grabbing the season pack must be considered
an upgrade of the pilot episode.
This can be achieved through a [custom format](https://trash-guides.info/Sonarr/sonarr-collection-of-custom-formats/#season-pack).
[Import](https://trash-guides.info/Sonarr/sonarr-import-custom-formats/)
the custom format and [configure a quality profile](https://trash-guides.info/Sonarr/sonarr-setup-quality-profiles/)
to prefer it.

## How to use

### Host installation

If you installed _prefetcharr_ through `cargo`, you can get a description of the
command-line interface by running `prefetcharr --help`.

### Docker installation

Users utilizing Docker only need to start the container, e.g. using `docker
-compose up d  prefetcharr`.
Once the container is running, you may want to check the logs for errors. You
can do so by either calling `docker logs prefetcharr` or by checking the logging
directory you configured.
