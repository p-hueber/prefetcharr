# prefetcharr

Have [Sonarr][sonarr] automatically fetch the next episodes of the show you’re
watching on [Jellyfin][jellyfin]/[Emby][emby]/[Plex][plex].

## Details

_prefetcharr_ periodically polls your media server for active playback sessions
of TV shows.
It then checks whether a configured number of successive episodes is available.
If there are episodes missing, it asks _Sonarr_ to search for them.
Depending on the configuration, it searches the missing episodes individually or
tries to fetch all seasons that contain them.
If there are no more seasons left, the series is monitored for new seasons
instead.

## Build and install

To install, first ensure Rust is installed on your system by following the
instructions at [Install Rust][rust], then run:
```
cargo install --git https://github.com/p-hueber/prefetcharr
```

Or with docker compose:
```yml
services:
  prefetcharr:
    image: phueber/prefetcharr:latest
    container_name: prefetcharr
    environment:
      - RUST_LOG=prefetcharr=debug
      - |
        PREFETCHARR_CONFIG=
       
        interval = 900           # Polling interval in seconds
        log_dir = "/log"         # Logging directory
        prefetch_num = 2         # Number of episodes to make available in advance
        request_seasons = true   # Always request full seasons to prefer season packs
        connection_retries = 6   # Number of retries for the initial connection probing

        [media_server]
        type = "Jellyfin"                       # `Jellyfin`, `Emby` or `Plex`
        url = "http://example.com/jellyfin"     # Jellyfin/Emby/Plex baseurl
        api_key = "<YOUR KEY HERE>"             # Jellyfin/Emby API key or plex server token
        # users = [ "John", "12345", "Axel F" ] # Optional: Only monitor sessions for specific user IDs or names
        # libraries = [ "TV Shows", "Anime" ]   # Optional: Only monitor sessions for specific libraries

        [sonarr]
        url = "http://example.com/sonarr" # Sonarr baseurl
        api_key = "<YOUR KEY HERE>"       # Sonarr API key

    volumes:
      - /path/to/log/dir:/log

```

## Configuration

The configuration is written in [TOML][toml] format. When running `prefetcharr`
directly, you can pass the path of the configuration file using the `--config`
command-line flag. For the Docker container, provide the entire configuration
via the `PREFETCHARR_CONFIG` environment variable.
A complete example can be found in the installation instructions for
`docker-compose` above. 

### API keys

_prefetcharr_ needs two different API keys to do its job.

#### `sonarr.api_key`

Go to `Settings` -> `General` -> `Security` and copy the API key.

#### `media_server.api_key`

The key to use and how to obtain it differs on the type of media server you use:

#### Jellyfin

Log in as an administrator and go to `Administration` -> `Dashboard` ->
`Advanced` -> `Api Keys`. Add a new key or use an existing one.

#### Emby

Log in as an administrator, click on the gear on the top right and go to
`Advanced` -> `Api Keys`. Add a new key or use an existing one.

#### Plex

You need to [extract the server token][plex-token] from a configuration file and
use it as the API key.

### Upgrading pilots

If you want to store pilot episodes only, _prefetcharr_ can fetch the first
season for you on demand.  
This method works well for individual episodes but may encounter issues with
season packs.  
For this to function in _Sonarr_, grabbing the season pack must be considered
an upgrade of the pilot episode.
This can be achieved through a [custom format][custom-format].
[Import][format-import] the custom format and
[configure a quality profile][quality-profile] to prefer it.

## How to use

### Host installation

If you installed _prefetcharr_ through `cargo`, you can get a description of the
command-line interface by running `prefetcharr --help`.

### Docker installation

Users utilizing Docker only need to start the container, e.g. using `docker
compose up -d prefetcharr`.
Once the container is running, you may want to check the logs for errors. You
can do so by either calling `docker logs prefetcharr` or by checking the logging
directory you configured.


[sonarr]: <https://sonarr.tv>
[jellyfin]: <https://jellyfin.org>
[emby]: <https://emby.media>
[plex]: <https://www.plex.tv>
[rust]: <https://www.rust-lang.org/tools/install>
[toml]: <https://toml.io/en/>
[plex-token]: <https://www.plexopedia.com/plex-media-server/general/plex-token/#plexservertoken>
[custom-format]: <https://trash-guides.info/Sonarr/sonarr-collection-of-custom-formats/#season-pack>
[format-import]: <https://trash-guides.info/Sonarr/sonarr-import-custom-formats/>
[quality-profile]: <https://trash-guides.info/Sonarr/sonarr-setup-quality-profiles/>
